// The page works without this file: every step is then shown one under the
// other and the download buttons lead to the release page.
(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  document.documentElement.classList.add('js');

  // Run the highlighter over an element once more.
  function light(el) {
    if (!el) return;
    el.classList.remove('lit');
    void el.offsetWidth;
    el.classList.add('lit');
  }

  // ---- the eight steps: a tab list ----
  const tabs = $$('.steps [role=tab]');
  const panels = tabs.map(tab => document.getElementById(tab.getAttribute('aria-controls')));
  function show(index, focus) {
    tabs.forEach((tab, i) => {
      const on = i === index;
      tab.setAttribute('aria-selected', on);
      tab.tabIndex = on ? 0 : -1;
      panels[i].hidden = !on;
    });
    if (focus) tabs[index].focus();
    const list = tabs[index].parentElement;
    if (list.scrollWidth > list.clientWidth) list.scrollTo({ left: tabs[index].offsetLeft - list.clientWidth / 2 + tabs[index].offsetWidth / 2, behavior: 'smooth' });
  }
  tabs.forEach((tab, i) => {
    tab.addEventListener('click', () => show(i));
    tab.addEventListener('keydown', event => {
      const step = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[event.key];
      const to = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : step ? (i + step + tabs.length) % tabs.length : null;
      if (to === null) return;
      event.preventDefault();
      show(to, true);
    });
  });
  if (tabs.length) {
    show(0);
    // The other screenshots, once the page has settled, so a step opens at once.
    addEventListener('load', () => setTimeout(() => panels.forEach(panel => $$('img', panel).forEach(img => { new Image().src = img.src; })), 1200));
  }

  // ---- citations of this page's own figures ----
  $$('a.cite').forEach(link => link.addEventListener('click', event => {
    const target = document.getElementById(link.hash.slice(1));
    if (!target) return;
    event.preventDefault();
    const step = link.dataset.step;
    if (step) show(tabs.findIndex(tab => tab.id === 'tab-' + step));
    (step ? $('#fig3') : target).scrollIntoView({ behavior: 'smooth', block: step ? 'start' : 'center' });
    setTimeout(() => light($('figcaption', target)), 450);
  }));

  // ---- 图 4: a suggestion, and the person's decision ----
  const verdict = $('#screen-verdict');
  const chip = $('#screen-chip');
  $$('.choices button').forEach(button => button.addEventListener('click', () => {
    $$('.choices button').forEach(other => other.setAttribute('aria-pressed', other === button));
    const label = button.dataset.label;
    chip.className = 'chip ' + button.dataset.tone;
    chip.textContent = '收录：' + label;
    verdict.textContent = button.classList.contains('suggested')
      ? '记下了：' + label + '。你采纳了助手的建议，判断理由沿用它写的那一段，之后在文献详情里随时可以改。'
      : '记下了：' + label + '。助手建议的是「相关」，你另选了一个，以你的为准。';
    light(verdict);
  }));

  // ---- 图 5: a citation that opens the page and marks the sentence ----
  $$('button.cite[data-quote]').forEach(button => button.addEventListener('click', () => {
    $$('button.cite[data-quote]').forEach(other => other.setAttribute('aria-pressed', other === button));
    $$('.sheet mark').forEach(mark => mark.classList.toggle('on', mark.id === button.dataset.quote));
    const mark = document.getElementById(button.dataset.quote);
    const box = mark.getBoundingClientRect();
    if (box.top < 80 || box.bottom > innerHeight - 40) mark.scrollIntoView({ behavior: 'smooth', block: 'center' });
  }));

  // ---- downloads: this visitor's system, and the latest release ----
  const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '';
  const agent = navigator.userAgent || '';
  const mobile = /Android|iPhone|iPad|iPod/.test(agent) || (/Mac/.test(platform) && navigator.maxTouchPoints > 1);
  const os = mobile ? '' : /Mac/i.test(platform) ? 'mac' : /Win/i.test(platform) ? 'win' : /Linux/i.test(platform) ? 'linux' : '';
  if (os) $(`.dl[data-os=${os}]`).classList.add('mine');

  // release.json is written when the site is deployed (see .github/workflows/pages.yml).
  // Earlier releases had one universal DMG; both Mac buttons lead to it there.
  const WANTED = { mac: [/macos.*arm64/, /macos-universal/], 'mac-intel': [/macos.*x64/, /macos-universal/], win: [/windows.*\.zip$/], 'win-setup': [/windows.*setup\.exe$/], linux: [/linux/] };
  fetch('release.json').then(response => response.ok ? response.json() : null).then(release => {
    if (!release || !release.tag || !Array.isArray(release.assets)) return;
    const version = $('[data-version]');
    version.textContent = release.tag;
    version.hidden = false;
    $('#hero-download').textContent = '下载 ' + release.tag;
    const day = release.published ? new Date(release.published).toLocaleDateString('zh-CN', { year: 'numeric', month: 'long', day: 'numeric' }) : '';
    const line = $('#release-line');
    line.textContent = `最新版本 ${release.tag}${day ? '，' + day + '发布' : ''}。`;
    const notes = document.createElement('a');
    notes.href = release.url;
    notes.textContent = '这一版改了什么';
    line.append(notes, '。');
    for (const [key, patterns] of Object.entries(WANTED)) {
      const asset = patterns.map(pattern => release.assets.find(a => pattern.test(a.name))).find(Boolean);
      if (!asset) {
        // A release from before there was a setup: nothing is offered that it does not have.
        if (key === 'win-setup') $$(`[data-asset="${key}"], [data-file="${key}"], [data-with="${key}"]`).forEach(el => el.remove());
        continue;
      }
      const kind = (asset.name.match(/\.(dmg|zip|tar\.gz|exe)$/) || [])[1];
      const button = $(`[data-asset="${key}"]`);
      button.href = asset.url;
      const label = button.dataset.label ? button.dataset.label + ' ' : '';
      button.textContent = label + (kind ? '下载 .' + kind : '下载');
      $(`[data-file="${key}"]`).textContent = `${asset.name}　${(asset.size / 1048576).toFixed(1)} MB`;
    }
  }).catch(() => {});
})();
