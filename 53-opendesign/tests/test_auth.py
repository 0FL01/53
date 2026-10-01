"""R17 auth / current R18 text reference QA. All values synthetic; no core/network."""
import json
import shutil
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
HTML = (ROOT / 'prototype.html').read_text()
assert (ROOT / 'prototype.html').read_bytes() == (ROOT / 'index.html').read_bytes()
RESULTS = {'checks': [], 'geometry': [], 'errors': [], 'requests': []}
GEOMETRY = """() => {
  const app = document.getElementById('app'), bounds = app.getBoundingClientRect();
  const elements = [...app.querySelectorAll('*')];
  const visible = e => {const r=e.getBoundingClientRect(); return r.width && r.height};
  return {
    overflow: app.scrollWidth > app.clientWidth + 1,
    small: elements.filter(e => e.matches('button,input,textarea,summary') && visible(e))
      .filter(e => {const r=e.getBoundingClientRect(); return r.width<47.9 || r.height<47.9})
      .map(e => e.id || e.tagName),
    horizontal: elements.filter(e => e.matches('button,input,textarea,summary') && visible(e))
      .filter(e => {const r=e.getBoundingClientRect(); return r.left<bounds.left-1 || r.right>bounds.right+1})
      .map(e => e.id || e.tagName),
    rounded: elements.filter(e => getComputedStyle(e).borderTopLeftRadius !== '0px').length,
    shadows: elements.filter(e => getComputedStyle(e).boxShadow !== 'none').length,
    clipped: elements.filter(e => e.matches('h2,h3,p,label,.value,.notice,.btn span') && visible(e))
      .filter(e => e.scrollWidth>e.clientWidth+1 || e.scrollHeight>e.clientHeight+1)
      .map(e => e.id || e.tagName)
  };
}"""

with sync_playwright() as playwright:
    browser = playwright.chromium.launch(
        executable_path=shutil.which('chromium') or shutil.which('chromium-browser') or shutil.which('google-chrome'),
        headless=True, args=['--no-sandbox'])
    page = browser.new_page(viewport={'width': 1440, 'height': 1080})
    page.set_default_timeout(4000)
    page.on('pageerror', lambda error: RESULTS['errors'].append(str(error)))
    page.on('request', lambda request: RESULTS['requests'].append(request.url))

    def reset():
        page.goto('about:blank')
        page.set_content(HTML, wait_until='load')
        page.evaluate('C.autoAnswer=false;setMode(false)')

    def evaluate(script):
        return page.evaluate(script)

    def check(name, expression):
        ok = bool(evaluate(expression))
        RESULTS['checks'].append({'name': name, 'ok': ok})
        if not ok:
            print('FAILED', name, flush=True)

    def button(name):
        return page.locator('#app').get_by_role('button', name=name, exact=True)

    def fill_credentials(login='Demo.User', password=' demo-password '):
        page.locator('#account-login').fill(login)
        page.locator('#account-password').fill(password)

    reset()
    check('First launch starts with public server selection', "S.screen==='welcome' && A.requests.length===0")
    button('Вставить код сервера').click()
    page.locator('#profile-code').fill('not-a-profile')
    button('Показать данные сервера').click()
    check('Invalid input does not configure or contact server', "A.error==='BadQr' && A.requests.length===0 && !A.accepted")
    button('Вставить демо-образец').click()
    button('Показать данные сервера').click()
    check('Paste is offline preview, not auth', "S.screen==='preview' && A.preview && !A.accepted && A.requests.length===0 && A.mutations===0")
    button('Отмена').click()
    check('Preview cancel discards profile without requests', "S.screen==='welcome' && !A.preview && !A.accepted && A.requests.length===0")
    button('Сканировать QR').click()
    button('Смоделировать сканирование').click()
    check('Server scan uses same offline preview', "S.screen==='preview' && A.requests.length===0")
    button('Использовать этот сервер').click()
    check('Explicit profile accept configures DNS then queries policy', "A.accepted && A.requests.map(r=>r.operation).join(',')==='configure-dns,registration-policy' && A.mutations===0")
    check('Unknown policy blocks signup with closed default', "A.policyState==='loading' && A.policy==='invite_only' && [...document.querySelectorAll('#app button[onclick^=authScreen]')].at(-1).disabled")
    page.wait_for_function("A.policyState==='ready'")
    check('Default synthetic policy resolves invite_only', "A.policy==='invite_only'")
    button('Войти').click()
    check('Login never shows invitation', "!document.getElementById('account-invitation')")
    fill_credentials()
    button('Показать пароль').click()
    check('Reveal is explicit and preserves exact password', "document.getElementById('account-password').type==='text' && A.password===' demo-password '")
    button('Скрыть пароль').click()
    check('Hide restores masked input', "document.getElementById('account-password').type==='password'")
    button('Войти').click()
    check('Password proof request omits invitation and expected old key', "A.busy && A.login==='demo.user' && A.password===' demo-password ' && A.requests.at(-1).operation==='login' && !A.requests.at(-1).invitationIncluded && A.requests.at(-1).expectedDevice===null")
    evaluate("submitAuth('login')")
    check('Pending login prevents duplicate submit', "A.requests.filter(r=>r.operation==='login').length===1")
    page.wait_for_function('A.authenticated')
    check('Success clears transient credentials', "S.screen==='chats' && !A.password && !A.invitation && !A.reveal && !A.challenge")
    evaluate('resumeDemo()')
    check('Resume is key-only', "A.requests.at(-1).operation==='key-resume' && !A.requests.at(-1).passwordIncluded && !A.password")
    evaluate("resumeDemo('Revoked')")
    check('Revoked resume does not silently re-register', "S.screen==='auth' && !A.authenticated && A.error==='Revoked' && A.requests.at(-1).operation==='key-resume'")

    reset()
    evaluate("S.pendingContact=true;go('scan');scanDemo()")
    check('Contact scanner is separate from server import', "S.screen==='contact' && !A.accepted && A.requests.length===0 && S.chatState==='requested'")
    evaluate("go('scan');permissionDemo('camera')")
    button('Добавить по ID').click()
    check('Denied contact camera cannot become server auth', "S.screen==='add' && !A.accepted && A.requests.length===0")
    reset()
    evaluate("go('scan');permissionDemo('camera')")
    button('Вставить код сервера').click()
    check('Denied camera has working paste fallback', "S.screen==='import' && !document.querySelector('[role=dialog]')")
    evaluate('previewProfileDemo()')
    button('Назад').click()
    check('Preview back is cancel without configuration', "S.screen==='welcome' && !A.preview && !A.accepted && A.requests.length===0")
    evaluate("previewProfileDemo();acceptProfileDemo();authScreen('login')")
    page.wait_for_function("A.policyState==='ready'")
    check('Policy reply remains scoped to profile after entering login', "S.screen==='login' && A.policy==='invite_only' && !A.authenticated")

    reset()
    evaluate("authScenario('signup','invite_only')")
    check('Invite-only signup shows separate invitation', "!!document.getElementById('account-invitation')")
    fill_credentials()
    button('Создать аккаунт').click()
    check('Missing invitation does not submit', "A.error==='InviteRequired' && !A.busy && A.requests.length===0")
    page.locator('#account-invitation').fill('DEMO-INVITATION')
    button('Создать аккаунт').click()
    check('Signup request includes invitation only in invite_only', "A.busy && A.requests.at(-1).operation==='signup' && A.requests.at(-1).invitationIncluded")
    evaluate("finishAuthDemo('success')")
    check('Signup activates key session without retaining invitation', "A.authenticated && A.invitation==='' && A.password===''")
    evaluate("authScenario('signup','open')")
    check('Open signup has no invitation field', "!document.getElementById('account-invitation')")
    fill_credentials()
    button('Создать аккаунт').click()
    check('Open signup request omits invitation', "A.busy && !A.requests.at(-1).invitationIncluded")
    evaluate("finishAuthDemo('challenge')")
    check('Signup cannot accept a login replacement challenge', "A.error==='Protocol' && S.screen==='signup' && !A.challenge")

    for login, password in [(' ab', 'demo-password'), ('ab', 'demo-password'), ('demo.user', 'short'), ('demo.user', '🙂' * 33), ('demo.user', 'demo\npassword')]:
        reset()
        evaluate("authScenario('login')")
        # Set controls via state for the control-character case; HTML single-line
        # inputs themselves strip newlines. Core must still validate the payload.
        page.evaluate('([login,password])=>{A.login=login;A.password=password;submitAuth("login")}', [login, password])
        check('Auth byte/control/login validation', "A.error==='InvalidInput' && A.requests.length===0")
    reset()
    evaluate("authScenario('login')")
    fill_credentials(password='🙂' * 32)
    button('Войти').click()
    check('128 UTF-8 bytes are accepted without Unicode normalization', "A.busy && new TextEncoder().encode(A.password).length===128")
    evaluate("cancelAuthDemo();finishAuthDemo('success')")
    page.wait_for_timeout(650)
    check('Canceled attempt ignores late callback', "S.screen==='auth' && !A.authenticated && A.mutations===0 && !A.password")

    reset()
    evaluate("previewProfileDemo();acceptProfileDemo();finishPolicyDemo('Transport')")
    check('Policy failure stays closed and exposes retry', "A.policyState==='error' && A.policy==='invite_only' && [...document.querySelectorAll('#app button[onclick^=authScreen]')].at(-1).disabled")
    button('Повторить проверку').click()
    page.wait_for_function("A.policyState==='ready'")
    check('Policy retry is explicit and succeeds in fixture', "A.requests.filter(r=>r.operation==='registration-policy').length===2")
    evaluate("finishPolicyDemo('PinMismatch')")
    check('Pin mismatch blocks auth rather than offering bypass', "A.pinBlocked && document.querySelector('#app button[onclick^=authScreen]').disabled")

    reset()
    evaluate("authScenario('login');A.outcome='challenge'")
    fill_credentials()
    button('Войти').click()
    page.wait_for_function("S.screen==='replace'")
    check('Fresh-key login returns challenge without mutation', "A.challenge && A.requests.length===1 && A.mutations===0 && !A.authenticated")
    button('Отмена · оставить прежнее устройство').click()
    evaluate('confirmReplacementDemo()')
    check('Cancel challenge preserves old binding and sends no second LOGIN', "A.requests.length===1 && A.mutations===0 && !A.challenge && !A.password")
    evaluate("authScenario('replace')")
    button('Назад').click()
    check('Back is also replacement cancel', "S.screen==='auth' && !A.challenge && !A.password && A.mutations===0")
    evaluate("authScenario('replace')")
    button('Заменить устройство').click()
    check('Only explicit confirm sends expected-old key', "A.busy && A.requests.at(-1).operation==='login' && A.requests.at(-1).expectedDevice==='DEMO-OLD-DEVICE-KEY' && A.mutations===0")
    evaluate('confirmReplacementDemo();cancelAuthDemo()')
    check('Confirm pending blocks repeat and cancel-after-submit claim', "A.requests.length===2 && A.busy && S.screen==='replace'")
    evaluate("finishAuthDemo('ReplacementChanged')")
    check('CAS conflict discards challenge and requires new proof', "S.screen==='login' && A.error==='ReplacementChanged' && !A.challenge && !A.password && A.mutations===0")
    evaluate("authScenario('replace');confirmReplacementDemo();finishAuthDemo('success')")
    check('Confirmed replacement completes once then clears challenge', "A.authenticated && A.mutations===1 && !A.challenge && !A.password")
    evaluate('finishAuthDemo();confirmReplacementDemo()')
    check('Late/duplicate confirmation cannot mutate again', "A.mutations===1 && A.requests.length===3")
    check('Demo operation traces contain no credential/profile payloads', "A.requests.every(r=>!('password' in r)&&!('invitation' in r)&&!('profile' in r))")

    errors = evaluate('Object.keys(AUTH_ERRORS)')
    for error in errors:
        screen = 'import' if error == 'BadQr' else 'signup' if error.startswith('Invite') or error == 'LoginTaken' else 'auth' if error == 'PinMismatch' else 'login'
        evaluate(f"authScenario('{screen}','invite_only','{error}')")
        check('Safe typed error: ' + error, f"document.querySelector('[role=alert]').dataset.error==='{error}' && !document.querySelector('[role=alert]').textContent.includes('demo-password')")

    reset()
    evaluate("setMode(true);demoCall('active');C.videoEnabled=true;setMode(false);go('chat')")
    check('Current mode hides media controls and lab', "!callLive() && !document.querySelector('#calls-lab') && document.getElementById('calls-board-button').hidden && !document.querySelector('#app .call-event') && !document.querySelector('#app button[aria-label^=Аудиозвонок]')")
    evaluate("go('video-preview');showCallsBoard()")
    check('Current mode blocks media deep link and board', "S.screen==='chats' && !document.body.classList.contains('board-mode')")
    evaluate("go('connection');document.querySelector('#app details').open=true")
    check('Connection uses DNS and no manual transport/pin inputs', "document.getElementById('app').textContent.includes('DNS') && !document.querySelector('#app input,#app textarea') && !document.getElementById('app').textContent.includes('DirectTcp')")
    check('Current mode separates synthetic R18 data from verified native acceptance', "document.getElementById('mode-note').textContent.includes('R18 encrypted bidirectional history') && document.getElementById('mode-note').textContent.includes('реализованы') && document.getElementById('mode-note').textContent.includes('синтетические') && document.getElementById('mode-note').textContent.includes('R19 native acceptance verified отдельно')")

    # Supported text fields are visible in current mode; samples are not a core
    # integration test. Media guards above must still hold with these fields.
    evaluate("go('chats')")
    check('Current summaries show local alias, preview, time and unread', "document.querySelector('.chatrow-name').textContent==='Анна' && document.querySelector('.chatrow-preview').textContent.includes('Напишу вечером') && document.querySelector('.chatrow time').title==='Локальное время этого устройства' && document.querySelector('.chatrow .unread').textContent==='2'")
    evaluate("window.savedPeer={...peerData[0]};Object.assign(peerData[0],{name:null,preview:null,time:null,unread:0});render()")
    check('Optional alias/preview/time use ID and empty-dialog fallback', "document.querySelector('.chatrow-name').textContent==='7K3M-P9TX-4V2N' && document.querySelector('.chatrow-preview').textContent==='Сообщений пока нет' && !document.querySelector('.chatrow:first-child time,.chatrow:first-child .unread')")
    evaluate("Object.assign(peerData[0],savedPeer);go('contact')")
    check('Local alias does not replace public contact ID', "document.querySelector('.profilehead h3').textContent==='Анна' && document.querySelector('.profilehead .mono').textContent==='7K3M-P9TX-4V2N'")
    evaluate("go('chat')")
    check('Current history shows both directions with local times and exact states', "document.querySelectorAll('#chatlog .message:not(.out)').length===3 && document.querySelectorAll('#chatlog .message.out').length===2 && document.querySelectorAll('#chatlog .message time').length===5 && !!document.querySelector('[data-delivery-state=delivered]') && !!document.querySelector('[data-delivery-state=accepted]')")
    check('Newest-first history fixture is reversed for chronological bubbles', "[...document.querySelectorAll('#chatlog .message p')].map(p=>p.textContent).join('|')===DEMO_HISTORY.slice().reverse().map(m=>m.text).join('|') && DEMO_HISTORY.map(m=>m.localId).join(',')==='5,4,3,2,1'")
    check('Incoming rows have no outgoing delivery state or receipt', "[...document.querySelectorAll('#chatlog .message:not(.out)')].every(m=>!m.hasAttribute('data-delivery-state') && !m.querySelector('.message-meta svg'))")
    evaluate("S.network='off';render()")
    page.locator('#composer-text').fill('Синтетический исходящий текст')
    button('Отправить сообщение').click()
    check('Current offline send appears as queued outgoing history', "S.sent.length===1 && document.querySelector('[data-delivery-state=queued] p').textContent==='Синтетический исходящий текст' && !document.getElementById('composer-text').value")
    evaluate("S.network='ok';S.sent.push({peer:0,text:'Демо доставлено',time:'09:42',status:'delivered'});retryDemo()")
    check('Retry keeps synthetic history unique and delivery monotonic', "S.sent.length===2 && S.sent[0].status==='accepted' && S.sent[1].status==='delivered'")
    evaluate("S.sent.push({peer:0,text:'Демо неизвестный статус',time:'09:43',status:null});render()")
    check('Unknown outgoing status never becomes delivered', "document.querySelector('[data-delivery-state=unknown]').textContent.includes('Статус пока неизвестен') && !document.querySelector('[data-delivery-state=unknown] svg') && !document.querySelector('[data-delivery-state=unknown]').textContent.includes('Доставка подтверждена')")
    evaluate("go('queue')")
    check('Queue describes current exact-status API without inferring absence', "document.getElementById('app').textContent.includes('R18 historyPage/messageStatus') && document.getElementById('app').textContent.includes('отсутствие строки в очереди не подтверждает доставку')")

    # Every newly introduced screen, busy/closed-policy state, and safe error copy
    # at three Android-width references and two text scales in BOTH modes.
    fixtures = {
        'welcome': "cancelProfileDemo()",
        'paste': "A.error=null;go('import')",
        'scan-server': "S.pendingContact=false;go('scan')",
        'scan-contact': "S.pendingContact=true;go('scan')",
        'preview': 'previewProfileDemo()',
        'auth-open': "authScenario('auth','open')",
        'auth-invite-only': "authScenario('auth')",
        'policy-loading': "authScenario('auth');A.policyState='loading';render()",
        'policy-error': "authScenario('auth');A.policyState='error';A.error='Transport';render()",
        'login': "authScenario('login')",
        'login-busy': "authScenario('login');A.busy=true;render()",
        'signup-open': "authScenario('signup','open')",
        'signup-invite-only': "authScenario('signup')",
        'signup-busy': "authScenario('signup');A.busy=true;render()",
        'replace': "authScenario('replace')",
        'replace-busy': "authScenario('replace');A.busy=true;render()",
        'replace-stale': "authScenario('replace');clearCredentials();render()",
        'connection-expanded': "authScenario('connection');document.querySelector('#app details').open=true",
        'camera-denied': "authScenario('scan');S.pendingContact=false;render();permissionDemo('camera')",
        'current-summaries': "authScenario('chats')",
        'bidirectional-history': "authScenario('chat');S.chatState='normal';S.sent=[];render()",
        'history-unknown': "authScenario('chat');S.sent=[{peer:0,text:'Демо — статус неизвестен',time:'09:43',status:null}];render()",
        'exact-status-queue': "authScenario('queue')",
    }
    for error in errors:
        screen = 'import' if error == 'BadQr' else 'signup' if error.startswith('Invite') or error == 'LoginTaken' else 'auth' if error == 'PinMismatch' else 'login'
        fixtures['error/' + error] = f"authScenario('{screen}','invite_only','{error}')"
    reset()
    for target in [False, True]:
        for name, script in fixtures.items():
            for width in [360, 390, 412]:
                for scale in [1, 2]:
                    # Clear fixture pending before navigation, without simulating
                    # a cancellation of a real request (there are no requests).
                    evaluate(f"A.busy=false;setMode({str(target).lower()});document.getElementById('live-device').style.width='{width}px';document.documentElement.style.setProperty('--scale','{scale}');" + script)
                    geometry = evaluate(GEOMETRY)
                    RESULTS['geometry'].append({'mode': 'target' if target else 'contract', 'state': name, 'width': width, 'scale': scale, **geometry})
    page.set_viewport_size({'width': 360, 'height': 800})
    reset()
    evaluate("document.documentElement.style.setProperty('--scale','2');authScenario('signup')")
    page.screenshot(path=str(ROOT / 'mobile-auth-signup-360-large-text.png'))
    check('200% mobile signup has no document overflow', 'document.documentElement.scrollWidth<=360')
    button('Создать аккаунт').scroll_into_view_if_needed()
    check('200% signup primary action remains reachable', "(()=>{const r=document.querySelector('#app button[onclick^=submitAuth]').getBoundingClientRect();return r.top>=0&&r.bottom<=innerHeight})()")
    evaluate("authScenario('replace');document.querySelector('#app .scroll').scrollTop=0")
    page.screenshot(path=str(ROOT / 'mobile-auth-replace-360-large-text.png'))
    button('Отмена · оставить прежнее устройство').scroll_into_view_if_needed()
    check('200% replacement cancel remains reachable', "(()=>{const r=document.querySelector('#app .btn[onclick^=cancelAuthDemo]').getBoundingClientRect();return r.top>=0&&r.bottom<=innerHeight})()")
    (ROOT / 'qa-auth-results.json').write_text(json.dumps(RESULTS, ensure_ascii=False, indent=2))
    bad = [g for g in RESULTS['geometry'] if any(g[key] for key in ['overflow', 'small', 'horizontal', 'rounded', 'shadows', 'clipped'])]
    print('GEOMETRY', len(RESULTS['geometry']), 'FAIL', len(bad))
    print(json.dumps(bad[:20], ensure_ascii=False, indent=2))
    print('FUNCTIONAL', len(RESULTS['checks']), 'FAIL', sum(not c['ok'] for c in RESULTS['checks']))
    print('JS_ERRORS', RESULTS['errors'], 'NETWORK_REQUESTS', RESULTS['requests'])
    browser.close()
    if bad or any(not c['ok'] for c in RESULTS['checks']) or RESULTS['errors'] or RESULTS['requests']:
        raise SystemExit(1)
