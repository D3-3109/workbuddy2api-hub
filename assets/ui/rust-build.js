/* Rust build adjustments, injected by the gateway after the original dashboard.
   The dashboard was written for the Python gateway; this build does not run
   the scheduler, daily tasks, desktop import, client one-click setup, quota
   probing or self-update. Their buttons stay in place but are disabled and
   say why, instead of failing with a red toast after the click. */
(function(){
  'use strict';
  /* The dashboard's translator walks text nodes and looks phrases up in its own
     dictionaries, which do not know these strings; each is given per language. */
  var TEXT = {
    note: {
      zh: '当前 Rust 版网关未实现此功能',
      'zh-Hant': '目前 Rust 版閘道未實作此功能',
      en: 'Not available in the Rust gateway'
    },
    tasks: {
      zh: '此 Rust 版网关不运行签到、成长任务与每日活跃等后台任务，以下数据不会更新。',
      'zh-Hant': '此 Rust 版閘道不執行簽到、成長任務與每日活躍等背景任務，以下資料不會更新。',
      en: 'The Rust gateway does not run check-ins, growth tasks or daily activity, so the figures below do not update.'
    },
    agents: {
      zh: '此 Rust 版网关不检测本机客户端，也不改写它们的配置文件。请按下方「其他客户端」的地址手动填写。',
      'zh-Hant': '此 Rust 版閘道不偵測本機用戶端，也不改寫它們的設定檔。請依下方「其他用戶端」的位址手動填寫。',
      en: 'The Rust gateway does not detect local clients or edit their config files. Enter the addresses under “Other clients” below by hand.'
    },
    maintenance: {zh: '后台维护: ', 'zh-Hant': '背景維護: ', en: 'Maintenance: '}
  };
  function lang(){
    try { return (window.WB_I18N && window.WB_I18N.current()) || 'zh'; } catch(e){ return 'zh'; }
  }
  function t(key){ var v = TEXT[key]; return v[lang()] || v.zh; }

  /* Buttons that call a handler whose endpoint this build answers with 501. */
  var UNSUPPORTED_HANDLERS = [
    'doDailyChat', 'doDailyChatPending', 'doDailyChatWeb', 'scanDesktop',
    'triggerBuddyCheckin', 'runGrowthTasks', 'triggerStreakBonus', 'triggerCatTravel',
    'syncProfile', 'applyAgent', 'restoreAgent', 'refreshPricingNow',
    'discoverProxySlots', 'testProxySlot', 'runQuotaScan', 'checkUpdateNow',
    'triggerSchedulerNow', 'toggleScheduler', 'rollbackToVersion', 'installUpdateNow'
  ];
  var HANDLER_RE = new RegExp('^\\s*(?:' + UNSUPPORTED_HANDLERS.join('|') + ')\\(');

  /* Whole sections that only describe unported features. */
  var UNSUPPORTED_SECTIONS = ['sectionIntlTasks', 'sectionGrowthTasks'];

  function mark(btn){
    if(btn.__wbUnsupported) return;
    btn.__wbUnsupported = true;
    btn.disabled = true;
    btn.classList.add('wb-unsupported');
    btn.setAttribute('title', t('note'));
    btn.setAttribute('aria-disabled', 'true');
  }

  function scan(root){
    var list = (root || document).querySelectorAll('button[onclick]');
    for(var i = 0; i < list.length; i++){
      if(HANDLER_RE.test(list[i].getAttribute('onclick') || '')) mark(list[i]);
    }
  }

  function notice(target, key){
    if(!target || target.querySelector(':scope > .wb-notice')) return;
    var box = document.createElement('div');
    box.className = 'wb-notice';
    box.setAttribute('role', 'note');
    box.setAttribute('data-no-i18n', '');
    box.setAttribute('data-wb-text', key);
    box.textContent = t(key);
    var head = target.firstElementChild;
    if(head && head.nextSibling) target.insertBefore(box, head.nextSibling);
    else target.appendChild(box);
  }

  function annotate(){
    UNSUPPORTED_SECTIONS.forEach(function(id){
      var sec = document.getElementById(id);
      if(!sec) return;
      sec.classList.add('wb-section-unsupported');
      notice(sec, 'tasks');
    });
    var agents = document.getElementById('agentCardGrid');
    if(agents && agents.parentElement){
      notice(agents.parentElement, 'agents');
    }
    var sched = document.getElementById('schedStatusText');
    if(sched){
      var label = sched.parentElement;
      if(label && label.firstChild && label.firstChild.nodeType === 3){
        label.setAttribute('data-wb-maint', '');
        label.firstChild.data = t('maintenance');
      }
    }
  }

  /* Polling-driven errors that only describe the missing features. */
  var QUIET = [/^智能体列表加载失败/, /^积分获取历史加载失败/, /^读取签到记录失败/, /^读取备份列表失败/];
  var TONES = {bad: '--bad', err: '--bad', error: '--bad', warn: '--warn', warning: '--warn', ok: '--accent2', success: '--accent2'};
  var originalToast = window.toast;
  if(typeof originalToast === 'function'){
    window.toast = function(msg, kind){
      var text = String(msg == null ? '' : msg);
      if((kind === 'bad' || kind === 'err' || kind === 'error') && /\b501\b/.test(text)){
        for(var i = 0; i < QUIET.length; i++) if(QUIET[i].test(text)) return;
      }
      var result = originalToast.apply(this, arguments);
      var box = document.getElementById('toastBox');
      var item = box && box.lastElementChild;
      if(item) item.style.setProperty('--wb-toast-tone', 'var(' + (TONES[kind] || '--accent') + ')');
      return result;
    };
  }

  /* Some handlers re-enable their button in a finally block or on the next poll;
     the class keeps the button inert even when `disabled` is reset. */
  document.addEventListener('click', function(e){
    var btn = e.target && e.target.closest ? e.target.closest('button.wb-unsupported') : null;
    if(!btn) return;
    e.preventDefault();
    e.stopImmediatePropagation();
  }, true);

  /* Re-render our own strings when the language changes. */
  function relabel(){
    document.querySelectorAll('[data-wb-text]').forEach(function(el){
      el.textContent = t(el.getAttribute('data-wb-text'));
    });
    document.querySelectorAll('button.wb-unsupported').forEach(function(b){ b.setAttribute('title', t('note')); });
    var label = document.querySelector('[data-wb-maint]');
    if(label && label.firstChild && label.firstChild.nodeType === 3) label.firstChild.data = t('maintenance');
  }
  if(window.WB_I18N && typeof window.WB_I18N.set === 'function'){
    var setLang = window.WB_I18N.set;
    window.WB_I18N.set = function(){ var r = setLang.apply(this, arguments); relabel(); return r; };
  }
  ['selectLang', 'toggleLang'].forEach(function(name){
    var fn = window[name];
    if(typeof fn !== 'function') return;
    window[name] = function(){ var r = fn.apply(this, arguments); relabel(); return r; };
  });

  function boot(){
    document.documentElement.setAttribute('data-runtime', 'rust');
    scan(document);
    annotate();
    if(typeof MutationObserver !== 'undefined'){
      new MutationObserver(function(records){
        for(var i = 0; i < records.length; i++){
          var added = records[i].addedNodes;
          for(var j = 0; j < added.length; j++){
            var n = added[j];
            if(n.nodeType !== 1) continue;
            if(n.tagName === 'BUTTON'){ if(HANDLER_RE.test(n.getAttribute('onclick') || '')) mark(n); }
            else if(n.querySelectorAll) scan(n);
          }
        }
      }).observe(document.body, {childList: true, subtree: true});
    }
  }
  if(document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot);
  else boot();
})();
