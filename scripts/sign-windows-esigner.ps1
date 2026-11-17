# 用 SSL.com eSigner CodeSignTool 给一个 Windows 可执行文件做 Authenticode 签名。
#
# scripts/package-windows.ps1 -Sign 为每个 exe 调用本脚本一次（WispPaper.exe、wisp.exe 和安装程序）。
# 每次签名都消耗一个 eSigner TOTP，所以连续两次签名之间会等到下一个 30 秒窗口。
#
# 用法：
#   pwsh -File scripts/sign-windows-esigner.ps1 -Install        # 下载并配置 CodeSignTool（CI 里先跑一次）
#   pwsh -File scripts/sign-windows-esigner.ps1 -Path app.exe   # 原地签名并校验
#
# 需要的环境变量（GitHub Actions secrets，不要写进仓库）：
#   ESIGNER_CREDENTIAL_ID  eSigner 签名凭证 id
#   ESIGNER_USERNAME       SSL.com 账号
#   ESIGNER_PASSWORD       SSL.com 密码
#   ESIGNER_TOTP_SECRET    CodeSignTool 用来算 OTP 的 TOTP 种子
#   ESIGNER_PIN            4 位 eSigner PIN（只做完整性检查，不会传给 CodeSignTool）
#
# 脚本来自 BiblioCell，沿用 wisp-science / wispterm 的经验：
# - CodeSignTool v1.3.2 的 main() 吞掉了 picocli 的退出码，认证失败、OTP 无效时 java 仍然返回 0。
#   成功的判定是退出码 0 + 输出里有 "Code signed successfully:" + 输出目录里有签好的文件。
# - SSL.com 默认时间戳服务 http://ts.ssl.com 发 ECDSA 时间戳，Windows 会拒绝
#   （"timestamp certificate does not meet a minimum public key length"），所以把 TSA_URL
#   固定为 RSA 的 http://ts.ssl.com/legacy。
# - OTP 无效时等下一个 TOTP 窗口重试，最多 4 次。失败时打印脱敏后的 stdout/stderr/日志。

param(
    [string]$Path,
    [switch]$Install
)

$ErrorActionPreference = 'Stop'

$script:CodeSignToolVersion = '1.3.2'
$script:CodeSignToolUrl = 'https://github.com/SSLcom/CodeSignTool/releases/download/v1.3.2/CodeSignTool-v1.3.2-windows.zip'
$script:CodeSignToolSha256 = '4afc32e8b7f79bbe1de7e4e7049aaad4e0f754357613b9bbec0e3052f06fd36b'
$script:TimestampUrl = 'http://ts.ssl.com/legacy'
$script:ProgramName = 'Wisp Paper'
$script:SecretNames = @(
    'ESIGNER_CREDENTIAL_ID',
    'ESIGNER_USERNAME',
    'ESIGNER_PASSWORD',
    'ESIGNER_TOTP_SECRET',
    'ESIGNER_PIN'
)
$script:CodeSignTimeoutSeconds = 600
$script:OtpAttemptLimit = 4
# 35 秒刚好跨过当前的 30 秒 TOTP 窗口。
$script:OtpRetrySeconds = 35
$script:TotpWindowSeconds = 31

# ---------- 公共 ----------

function Get-StateDir {
    $dir = $env:RUNNER_TEMP
    if ([string]::IsNullOrWhiteSpace($dir)) { $dir = [IO.Path]::GetTempPath() }
    return (Join-Path $dir 'wisppaper-esigner')
}

function Hide-Secrets([string]$Text) {
    if ([string]::IsNullOrEmpty($Text)) { return '' }
    $pairs = @()
    foreach ($name in $script:SecretNames) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if (-not [string]::IsNullOrEmpty($value) -and $value.Length -ge 4) {
            $pairs += [pscustomobject]@{ Name = $name; Value = $value }
        }
    }
    # 先替换长的，避免短密钥是长密钥子串时把长的切开露出一部分。
    foreach ($pair in @($pairs | Sort-Object { $_.Value.Length } -Descending)) {
        $Text = $Text.Replace([string]$pair.Value, ('***' + $pair.Name + '***'))
    }
    return $Text
}

function Assert-SecretsPresent {
    $missing = @()
    foreach ($name in $script:SecretNames) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ([string]::IsNullOrWhiteSpace($value)) { $missing += $name }
        elseif ($value -match '[\r\n]') { throw "$name contains a newline. Refusing to pass it to CodeSignTool." }
    }
    if ($missing.Count -gt 0) {
        throw ('Missing eSigner secret(s): ' + ($missing -join ', ') + '. Add them to the repository secrets.')
    }
}

# ---------- 安装 ----------

function Set-CodeSignToolProperty([string]$PropertiesPath, [string]$Key, [string]$Value) {
    $lines = @()
    if (Test-Path -LiteralPath $PropertiesPath) { $lines = [IO.File]::ReadAllLines($PropertiesPath) }
    $replaced = $false
    $out = foreach ($line in $lines) {
        if ($line -match ('^\s*' + [regex]::Escape($Key) + '\s*=')) {
            if (-not $replaced) { "$Key=$Value"; $replaced = $true }
        } else { $line }
    }
    if (-not $replaced) { $out = @($out) + "$Key=$Value" }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [IO.File]::WriteAllLines($PropertiesPath, [string[]]$out, $utf8)
}

function Resolve-CodeSignTool([string]$Root) {
    $properties = @(Get-ChildItem -Path $Root -Recurse -Filter 'code_sign_tool.properties' |
        Where-Object { $_.Directory.Name -eq 'conf' -and $_.Directory.FullName -notmatch '__MACOSX' })
    if ($properties.Count -ne 1) {
        throw "Expected one conf/code_sign_tool.properties under $Root, found $($properties.Count)"
    }
    $toolHome = $properties[0].Directory.Parent.FullName
    $jar = Join-Path (Join-Path $toolHome 'jar') ("code_sign_tool-" + $script:CodeSignToolVersion + ".jar")
    $java = Join-Path (Join-Path (Join-Path $toolHome 'jdk-11.0.2') 'bin') 'java.exe'
    if (-not (Test-Path -LiteralPath $jar)) { throw "Missing $jar" }
    if (-not (Test-Path -LiteralPath $java)) { throw "Missing $java" }
    return [pscustomobject]@{ Home = $toolHome; Jar = $jar; Java = $java; Properties = $properties[0].FullName }
}

function Install-CodeSignTool {
    $cache = $env:RUNNER_TOOL_CACHE
    if ([string]::IsNullOrWhiteSpace($cache)) { $cache = Get-StateDir }
    $dest = Join-Path (Join-Path $cache 'CodeSignTool') $script:CodeSignToolVersion
    $jar = Join-Path (Join-Path $dest 'jar') ("code_sign_tool-" + $script:CodeSignToolVersion + ".jar")
    if (-not (Test-Path -LiteralPath $jar)) {
        $zip = Join-Path ([IO.Path]::GetTempPath()) ("CodeSignTool-v" + $script:CodeSignToolVersion + "-windows.zip")
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $script:CodeSignToolUrl -OutFile $zip
        $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $script:CodeSignToolSha256) {
            throw "CodeSignTool zip SHA-256 mismatch: expected $($script:CodeSignToolSha256), got $actual"
        }
        if (Test-Path -LiteralPath $dest) { Remove-Item -LiteralPath $dest -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        & tar.exe -xf $zip -C $dest
        if ($LASTEXITCODE -ne 0) { throw "tar exited $LASTEXITCODE" }
        Remove-Item -LiteralPath $zip -Force
    }
    $tool = Resolve-CodeSignTool $dest
    Set-CodeSignToolProperty $tool.Properties 'TSA_URL' $script:TimestampUrl
    $env:ESIGNER_CODESIGNTOOL_HOME = $tool.Home
    $env:ESIGNER_JAVA = $tool.Java
    if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_ENV)) {
        $utf8 = New-Object System.Text.UTF8Encoding $false
        [IO.File]::AppendAllText($env:GITHUB_ENV, "ESIGNER_CODESIGNTOOL_HOME=$($tool.Home)`nESIGNER_JAVA=$($tool.Java)`n", $utf8)
    }
    Write-Host ("CodeSignTool " + $script:CodeSignToolVersion + " ready at " + $tool.Home + " (TSA_URL=" + $script:TimestampUrl + ")")
}

# ---------- 签名 ----------

function Get-TotpStatePath { return (Join-Path (Get-StateDir) 'totp-last.txt') }

function Set-TotpTimestamp {
    $state = Get-TotpStatePath
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $state) | Out-Null
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [IO.File]::WriteAllText($state, ([DateTime]::UtcNow.ToString('o')), $utf8)
}

function Wait-TotpWindow {
    $state = Get-TotpStatePath
    if (Test-Path -LiteralPath $state) {
        $parsed = [DateTime]::MinValue
        if ([DateTime]::TryParse([IO.File]::ReadAllText($state).Trim(), [ref]$parsed)) {
            $elapsed = ([DateTime]::UtcNow - $parsed.ToUniversalTime()).TotalSeconds
            if ($elapsed -lt $script:TotpWindowSeconds) {
                $delay = [int][Math]::Ceiling($script:TotpWindowSeconds - $elapsed)
                Write-Host "Waiting $delay s for a fresh eSigner TOTP"
                Start-Sleep -Seconds $delay
            }
        }
    }
    Set-TotpTimestamp
}

function Add-SignLog([string]$FilePath, [string]$Thumbprint) {
    $log = $env:ESIGNER_SIGN_LOG
    if ([string]::IsNullOrWhiteSpace($log)) { $log = Join-Path (Get-StateDir) 'signed.log' }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $log) | Out-Null
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [IO.File]::AppendAllText($log, ("signed`t" + $FilePath + "`t" + $Thumbprint + "`n"), $utf8)
}

function Invoke-CapturedProcess([string]$FileName, [string[]]$ArgumentList, [string]$WorkingDirectory, [int]$TimeoutSeconds) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $FileName
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.RedirectStandardInput = $true
    $psi.CreateNoWindow = $true
    $psi.WorkingDirectory = $WorkingDirectory
    $utf8 = New-Object System.Text.UTF8Encoding $false
    $psi.StandardOutputEncoding = $utf8
    $psi.StandardErrorEncoding = $utf8
    # ArgumentList 让每个密钥保持为一个独立参数（需要 PowerShell 7）。
    foreach ($arg in $ArgumentList) { [void]$psi.ArgumentList.Add([string]$arg) }

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $psi
    [void]$process.Start()
    $process.StandardInput.Close()
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $timedOut = -not $process.WaitForExit($TimeoutSeconds * 1000)
    if ($timedOut) {
        try { $process.Kill() } catch {}
        try { $process.WaitForExit() } catch {}
    } else {
        $process.WaitForExit()
    }
    return @{
        ExitCode = $(if ($timedOut) { 124 } else { $process.ExitCode })
        StdOut   = [string]$stdoutTask.GetAwaiter().GetResult()
        StdErr   = [string]$stderrTask.GetAwaiter().GetResult()
        TimedOut = [bool]$timedOut
    }
}

function Read-CodeSignLog([string]$ToolHome) {
    $logPath = Join-Path (Join-Path $ToolHome 'logs') 'code_signing_tool.log'
    if (-not (Test-Path -LiteralPath $logPath)) { return '' }
    try { $text = [IO.File]::ReadAllText($logPath) } catch { return '' }
    if ($text.Length -gt 16384) { $text = "(log truncated)`n" + $text.Substring($text.Length - 16384) }
    return $text
}

function Clear-CodeSignLog([string]$ToolHome) {
    $logDir = Join-Path $ToolHome 'logs'
    if (Test-Path -LiteralPath $logDir) { Remove-Item -LiteralPath $logDir -Recurse -Force -ErrorAction SilentlyContinue }
}

function Get-Disposition([int]$ExitCode, [string]$Output, [bool]$SignedFilePresent) {
    if ([regex]::IsMatch($Output, '(?i)invalid\s+otp') -or [regex]::IsMatch($Output, '(?i)otp\s+is\s+invalid')) { return 'retry-otp' }
    $hasError = [regex]::IsMatch($Output, '(?im)(^|\r?\n)\s*Error\s*:') -or [regex]::IsMatch($Output, '(?i)(\bException\b|java\.lang\.)')
    $hasSuccess = [regex]::IsMatch($Output, '(?i)Code signed successfully\s*:')
    if ($ExitCode -eq 0 -and $SignedFilePresent -and $hasSuccess -and -not $hasError) { return 'success' }
    return 'fail'
}

function Invoke-Sign([string]$FilePath) {
    if ([string]::IsNullOrWhiteSpace($FilePath)) { throw 'Usage: sign-windows-esigner.ps1 -Path FILE' }
    if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) { throw "eSigner target does not exist: $FilePath" }
    # CodeSignTool 在自己的目录里运行，相对路径到那里就找不到了。
    $FilePath = (Resolve-Path -LiteralPath $FilePath).ProviderPath
    $ext = [IO.Path]::GetExtension($FilePath).ToLowerInvariant()
    # CodeSignTool 按扩展名识别文件类型。扩展名不是它认识的那几种时，先确认是 PE 文件（MZ 头），
    # 再用 .exe 副本签名，成功后写回原路径。
    $work = $FilePath
    if ($ext -notin @('.exe', '.dll', '.msi')) {
        $head = New-Object byte[] 2
        $stream = [IO.File]::OpenRead($FilePath)
        try { [void]$stream.Read($head, 0, 2) } finally { $stream.Dispose() }
        if (-not ($head[0] -eq 0x4D -and $head[1] -eq 0x5A)) { throw "Refusing to sign unsupported file type: $FilePath" }
        $work = Join-Path ([IO.Path]::GetTempPath()) ('esigner-' + [guid]::NewGuid().ToString('n') + '.exe')
        Copy-Item -LiteralPath $FilePath -Destination $work -Force
        Write-Host "Signing $([IO.Path]::GetFileName($FilePath)) as $([IO.Path]::GetFileName($work))"
    }

    Assert-SecretsPresent
    if ([string]::IsNullOrWhiteSpace($env:ESIGNER_CODESIGNTOOL_HOME) -or [string]::IsNullOrWhiteSpace($env:ESIGNER_JAVA)) {
        throw 'CodeSignTool is not installed. Run sign-windows-esigner.ps1 -Install first.'
    }
    $tool = Resolve-CodeSignTool $env:ESIGNER_CODESIGNTOOL_HOME
    $leaf = [IO.Path]::GetFileName($work)
    $succeeded = $false
    try {
        for ($attempt = 1; $attempt -le $script:OtpAttemptLimit; $attempt++) {
            $outputDir = Join-Path ([IO.Path]::GetTempPath()) ('esigner-' + [guid]::NewGuid().ToString('n'))
            New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
            try {
                if ($attempt -eq 1) { Wait-TotpWindow } else { Set-TotpTimestamp }
                Clear-CodeSignLog $tool.Home
                $javaArgs = @(
                    '-jar', $tool.Jar,
                    'sign',
                    ('-username=' + $env:ESIGNER_USERNAME),
                    ('-password=' + $env:ESIGNER_PASSWORD),
                    ('-credential_id=' + $env:ESIGNER_CREDENTIAL_ID),
                    ('-totp_secret=' + $env:ESIGNER_TOTP_SECRET),
                    ('-input_file_path=' + $work),
                    ('-output_dir_path=' + $outputDir),
                    ('-program_name=' + $script:ProgramName),
                    '-malware_block=false'
                )
                $captured = Invoke-CapturedProcess $tool.Java $javaArgs $tool.Home $script:CodeSignTimeoutSeconds
                $stdout = Hide-Secrets $captured.StdOut
                $stderr = Hide-Secrets $captured.StdErr
                $logText = Hide-Secrets (Read-CodeSignLog $tool.Home)
                $signed = @(Get-ChildItem -LiteralPath $outputDir -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -eq $leaf })
                $signedPresent = ($signed.Count -eq 1)
                $disposition = Get-Disposition -ExitCode ([int]$captured.ExitCode) -Output ($stdout + "`n" + $stderr) -SignedFilePresent $signedPresent
                if ($captured.TimedOut) { $disposition = 'fail' }
                if ($disposition -eq 'success') {
                    Move-Item -LiteralPath $signed[0].FullName -Destination $work -Force
                    $succeeded = $true
                    break
                }
                $reason = if ($captured.TimedOut) { 'timed out' } elseif (-not $signedPresent) { 'did not write a signed copy' } else { 'rejected the signed result' }
                $failure = "CodeSignTool failed to sign $leaf ($reason, exit $($captured.ExitCode)).`nstdout:`n$stdout`nstderr:`n$stderr`nlog:`n$logText"
                if ($disposition -eq 'retry-otp' -and $attempt -lt $script:OtpAttemptLimit) {
                    Write-Host "eSigner rejected the one-time password (attempt $attempt/$($script:OtpAttemptLimit)). Waiting $($script:OtpRetrySeconds)s for the next TOTP window."
                    Write-Host $failure
                    Start-Sleep -Seconds $script:OtpRetrySeconds
                    continue
                }
                throw $failure
            } finally {
                if (Test-Path -LiteralPath $outputDir) { Remove-Item -LiteralPath $outputDir -Recurse -Force }
            }
        }
        if (-not $succeeded) { throw "CodeSignTool failed to sign $leaf." }

        $signature = Get-AuthenticodeSignature -LiteralPath $work
        if ("$($signature.Status)" -ne 'Valid') { throw "$leaf Authenticode status $($signature.Status): $($signature.StatusMessage)" }
        if (-not $signature.TimeStamperCertificate) { throw "$leaf has no RFC3161 timestamp" }
        if ($work -ne $FilePath) { Move-Item -LiteralPath $work -Destination $FilePath -Force }
        Add-SignLog $FilePath $signature.SignerCertificate.Thumbprint
        Write-Host "Signed $([IO.Path]::GetFileName($FilePath)): $($signature.Status), $($signature.SignerCertificate.Subject), timestamped by $($signature.TimeStamperCertificate.Subject)"
    } finally {
        Clear-CodeSignLog $tool.Home
        if ($work -ne $FilePath -and (Test-Path -LiteralPath $work)) { Remove-Item -LiteralPath $work -Force -ErrorAction SilentlyContinue }
    }
}

try {
    if ($Install) {
        Install-CodeSignTool
    } else {
        Invoke-Sign $Path
    }
} catch {
    $message = [string]$_.Exception.Message
    if ([string]::IsNullOrWhiteSpace($message)) { $message = [string]$_.ToString() }
    Write-Host (Hide-Secrets $message)
    exit 1
}
# 调用方检查 $LASTEXITCODE；不显式 exit 时它是 $null。
exit 0
