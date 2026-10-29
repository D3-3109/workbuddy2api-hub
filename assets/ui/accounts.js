/* Account list: regroup each row's actions after the original renderer runs.
   The original cell is two rows of eight controls (~284px), the widest column
   in the table. Test and credential refresh stay visible; enable/disable, the
   outbound identity switch, nickname sync, export and delete move into a
   per-row menu. Enable state is already shown in the status column. Buttons
   are moved, not copied, so their inline handlers, disabled state and the
   Rust-build markers keep working. Desktop only; the phone card layout keeps
   the original arrangement. */
(function(){
  'use strict';
  var DESKTOP = window.matchMedia ? window.matchMedia('(min-width:641px)') : {matches: true};
  var TEXT = {
    more: {zh: '更多', 'zh-Hant': '更多', en: 'More'},
    identity: {zh: '出站身份', 'zh-Hant': '出站身分', en: 'Outbound identity'},
    weight: {zh: '权重', 'zh-Hant': '權重', en: 'Weight'}
  };
  function lang(){
    try { return (window.WB_I18N && window.WB_I18N.current()) || 'zh'; } catch(e){ return 'zh'; }
  }
  function t(key){ return TEXT[key][lang()] || TEXT[key].zh; }

  function handler(btn){
    var m = /^\s*(\w+)\(/.exec(btn.getAttribute('onclick') || '');
    return m ? m[1] : '';
  }

  function regroup(td){
    if(td.__wbGrouped) return;
    var col = td.querySelector('.acct-actions-col');
    if(!col) return;
    var buttons = Array.prototype.slice.call(col.querySelectorAll('button'));
    var find = function(name){ return buttons.filter(function(b){ return handler(b) === name; })[0]; };
    var toggle = find('toggleAccount');
    var test = find('testAccount');
    var refresh = find('refreshOne') || buttons.filter(function(b){ return handler(b) === 'openLoginModal'; })[0];
    var sync = find('syncProfile');
    var exp = find('exportOne');
    var del = find('deleteAccount');
    var ids = col.querySelector('.id-btn-group');
    if(!toggle || !test || !ids) return;

    var bar = document.createElement('div');
    bar.className = 'wb-acct-bar';
    [test, refresh].forEach(function(b){ if(b) bar.appendChild(b); });

    var menu = document.createElement('div');
    menu.className = 'wb-acct-menu';
    menu.setAttribute('role', 'menu');
    var idRow = document.createElement('div');
    idRow.className = 'wb-acct-menu-row';
    var idLabel = document.createElement('span');
    idLabel.className = 'wb-acct-menu-label';
    idLabel.setAttribute('data-wb-acct-text', 'identity');
    idLabel.setAttribute('data-no-i18n', '');
    idLabel.textContent = t('identity');
    idRow.appendChild(idLabel);
    idRow.appendChild(ids);
    menu.appendChild(idRow);
    // Weight mirror: a plain select bound to the row's own weight select, so
    // the original change handler (and its toast and re-render) still runs.
    var weightCell = td.parentElement && td.parentElement.children[5];
    var weightSel = weightCell && weightCell.querySelector('select');
    if(weightSel){
      var wRow = document.createElement('div');
      wRow.className = 'wb-acct-menu-row wb-acct-weight-row';
      var wLabel = document.createElement('span');
      wLabel.className = 'wb-acct-menu-label';
      wLabel.setAttribute('data-wb-acct-text', 'weight');
      wLabel.setAttribute('data-no-i18n', '');
      wLabel.textContent = t('weight');
      var mirror = document.createElement('select');
      mirror.className = 'mini-select wb-acct-weight';
      mirror.setAttribute('aria-label', t('weight'));
      mirror.innerHTML = weightSel.innerHTML;
      mirror.value = weightSel.value;
      mirror.addEventListener('change', function(){
        weightSel.value = mirror.value;
        weightSel.dispatchEvent(new Event('change', {bubbles: true}));
      });
      wRow.appendChild(wLabel);
      wRow.appendChild(mirror);
      menu.appendChild(wRow);
    }
    var rest = document.createElement('div');
    rest.className = 'wb-acct-menu-row wb-acct-menu-actions';
    [toggle, sync, exp, del].forEach(function(b){ if(b) rest.appendChild(b); });
    menu.appendChild(rest);

    var more = document.createElement('button');
    more.type = 'button';
    more.className = 'sec mini wb-acct-more';
    more.setAttribute('aria-haspopup', 'menu');
    more.setAttribute('aria-expanded', 'false');
    more.setAttribute('data-wb-acct-text', 'more');
    more.setAttribute('data-no-i18n', '');
    more.textContent = t('more');

    var holder = document.createElement('div');
    holder.className = 'wb-acct-more-wrap';
    holder.appendChild(more);
    holder.appendChild(menu);
    bar.appendChild(holder);

    col.innerHTML = '';
    col.appendChild(bar);
    td.__wbGrouped = true;

    // The list only ever shows one realm, so the realm column repeats the same
    // badge on every row. Its tier badge (Pro / Free) moves next to the name,
    // where it stays visible when narrow layouts hide that column.
    var tr = td.parentElement;
    var realmCell = tr && tr.children[0];
    var nameRow = tr && tr.querySelector('.acct-name-row');
    var tier = realmCell && realmCell.querySelector('.badge.mini');
    if(tier && nameRow && !nameRow.querySelector('.wb-acct-tier')){
      var copy = tier.cloneNode(true);
      copy.classList.add('wb-acct-tier');
      var name = nameRow.querySelector('.acct-name');
      if(name && name.nextSibling) nameRow.insertBefore(copy, name.nextSibling);
      else nameRow.appendChild(copy);
    }
  }

  /* Rows are rebuilt from scratch on every 15 s poll. A menu that vanishes
     under the pointer is worse than a briefly stale row, so a rebuild that
     arrives while a menu is open runs when it closes. */
  var openHolder = null;
  var pendingRender = false;
  var originalRender = window.renderAccounts;
  function render(){
    pendingRender = false;
    var r = originalRender.apply(window, arguments);
    apply();
    return r;
  }
  function close(){
    if(!openHolder) return;
    openHolder.classList.remove('open');
    var b = openHolder.querySelector('.wb-acct-more');
    if(b) b.setAttribute('aria-expanded', 'false');
    openHolder = null;
    if(pendingRender) render();
  }
  function open(holder){
    close();
    holder.classList.add('open');
    var b = holder.querySelector('.wb-acct-more');
    if(b) b.setAttribute('aria-expanded', 'true');
    // Open upwards when there is no room below, either in the viewport or in
    // the table wrapper, which clips vertically.
    var r = holder.getBoundingClientRect();
    var clip = holder.closest('.table-wrap');
    var limit = Math.min(window.innerHeight, clip ? clip.getBoundingClientRect().bottom : Infinity);
    holder.classList.toggle('up', limit - r.bottom < 150 && r.top > 150);
    openHolder = holder;
  }
  document.addEventListener('click', function(e){
    var more = e.target.closest && e.target.closest('.wb-acct-more');
    if(more){
      var holder = more.parentElement;
      if(holder === openHolder) close(); else open(holder);
      e.stopPropagation();
      return;
    }
    if(openHolder && !openHolder.contains(e.target)) close();
    else if(openHolder && e.target.closest && e.target.closest('.wb-acct-menu button')) setTimeout(close, 0);
  });
  document.addEventListener('keydown', function(e){
    if(e.key === 'Escape' && openHolder){
      var b = openHolder.querySelector('.wb-acct-more');
      close();
      if(b) b.focus();
    }
  });

  function apply(){
    if(!DESKTOP.matches) return;
    var cells = document.querySelectorAll('#accounts table.account-cards td.acct-actions');
    for(var i = 0; i < cells.length; i++) regroup(cells[i]);
  }

  if(typeof originalRender === 'function'){
    window.renderAccounts = function(){
      if(openHolder){ pendingRender = true; return; }
      return render.apply(this, arguments);
    };
  }
  // A viewport change across the breakpoint needs the original markup back.
  if(DESKTOP.addEventListener){
    DESKTOP.addEventListener('change', function(){
      if(typeof window.renderAccounts === 'function') window.renderAccounts();
    });
  }
  function relabel(){
    document.querySelectorAll('[data-wb-acct-text]').forEach(function(el){
      el.textContent = t(el.getAttribute('data-wb-acct-text'));
    });
  }
  ['selectLang', 'toggleLang'].forEach(function(name){
    var fn = window[name];
    if(typeof fn !== 'function') return;
    window[name] = function(){ var r = fn.apply(this, arguments); relabel(); return r; };
  });

  if(document.readyState === 'loading') document.addEventListener('DOMContentLoaded', apply);
  else apply();
})();
