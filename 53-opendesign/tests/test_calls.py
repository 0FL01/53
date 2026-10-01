import json
import shutil
from pathlib import Path
from playwright.sync_api import sync_playwright
ROOT=Path(__file__).resolve().parents[1]
html=(ROOT/'prototype.html').read_text()
results={'geometry':[],'checks':[],'errors':[],'requests':[]}
with sync_playwright() as p:
 b=p.chromium.launch(executable_path=shutil.which('chromium') or shutil.which('chromium-browser') or shutil.which('google-chrome'),headless=True,args=['--no-sandbox'])
 page=b.new_page(viewport={'width':1440,'height':1080})
 page.set_default_timeout(4000)
 page.on('pageerror',lambda e:results['errors'].append(str(e)))
 page.on('request',lambda r:results['requests'].append(r.url))
 def reset():
  page.goto('about:blank');page.set_content(html,wait_until='load');page.evaluate('C.autoAnswer=false')
 def check(name,expr):
  ok=page.evaluate(expr)
  results['checks'].append({'name':name,'ok':bool(ok)})
  if not ok:print('FAILED',name,flush=True)
 def ev(s):return page.evaluate(s)
 reset()
 base=[s[0] for s in ev('screens.filter(s=>!CALL_SCREENS.some(c=>c[0]===s[0]))')]
 allscreens=[s[0] for s in ev('screens')]
 for target in [True,False]:
  for screen in (allscreens if target else base):
   for width in [360,390,412]:
    for scale in [1,2]:
     ev(f"S.target={str(target).lower()};C.call=null;C.videoEnabled=false;C.historyState='normal';S.chatState='normal';S.network='ok';document.documentElement.style.setProperty('--scale','{scale}');document.getElementById('live-device').style.width='{width}px';go('{screen}')")
     g=ev("""(()=>{const app=document.getElementById('app'),r=app.getBoundingClientRect();return {
      overflow:app.scrollWidth>app.clientWidth+1,
      small:[...app.querySelectorAll('button')].filter(b=>{const r=b.getBoundingClientRect();return r.width&&r.height&&(r.width<47.9||r.height<47.9)}).map(b=>({text:b.textContent.trim().slice(0,35),w:b.getBoundingClientRect().width,h:b.getBoundingClientRect().height})),
      rounded:[...app.querySelectorAll('*')].filter(b=>getComputedStyle(b).borderTopLeftRadius!=='0px').length,
      horizontal:[...app.querySelectorAll('button,input,textarea')].filter(b=>{const q=b.getBoundingClientRect();return q.width&&(q.left<r.left-1||q.right>r.right+1)}).map(b=>b.textContent.trim().slice(0,30))
     }})()""")
     results['geometry'].append({'mode':'target' if target else 'contract','screen':screen,'width':width,'scale':scale,**g})
 # All major live and terminal call states with narrow / large text settings.
 for state in ['dialing','ringing','incoming','connecting','active','reconnecting','held','ended']:
  for video in [False,True]:
   for width in [360,390,412]:
    for scale in [1,2]:
     ev(f"C.videoEnabled={str(video).lower()};document.documentElement.style.setProperty('--scale','{scale}');document.getElementById('live-device').style.width='{width}px';demoCall('{state}',{{media:'{'video' if video else 'audio'}'}})")
     g=ev("""(()=>{const app=document.getElementById('app'),r=app.getBoundingClientRect();return {
      overflow:app.scrollWidth>app.clientWidth+1,
      small:[...app.querySelectorAll('button')].filter(b=>{const r=b.getBoundingClientRect();return r.width&&r.height&&(r.width<47.9||r.height<47.9)}).map(b=>({text:b.textContent.trim().slice(0,35),w:b.getBoundingClientRect().width,h:b.getBoundingClientRect().height})),
      rounded:[...app.querySelectorAll('*')].filter(b=>getComputedStyle(b).borderTopLeftRadius!=='0px').length,
      horizontal:[...app.querySelectorAll('button')].filter(b=>{const q=b.getBoundingClientRect();return q.width&&(q.left<r.left-1||q.right>r.right+1)}).map(b=>b.textContent.trim().slice(0,30))
     }})()""")
     results['geometry'].append({'screen':'call/'+state,'video':video,'width':width,'scale':scale,**g})
 reset();ev("go('chat')");check('Chat has future call button',"!!document.querySelector('#app button[aria-label^=Аудиозвонок]')")
 ev("setMode(false);go('chat')");check('Current API hides all call controls',"!document.querySelector('#app .call-event')&&!document.querySelector('#app button[aria-label^=Аудиозвонок]')&&!document.querySelector('#navigation button[onclick=\"go(\\'calls\\')\"]')")
 ev("go('call')");check('Current API prevents call screen deep link',"S.screen==='chats'&&!callLive()")
 reset();ev("go('chat');S.draft='Сохранить мой черновик';render();startCall(0)")
 check('Start is not an active conversation',"C.call.stage==='dialing'&&C.call.connectedAt===null")
 ev("toggleCallMute();transitionCall('ringing');connectCall();transitionCall('active')")
 check('Pre-call mute survives connect',"C.call.stage==='active'&&C.call.muted&&C.call.connectedAt!==null")
 ev('callBack()');check('Minimize preserves session and draft',"S.screen==='chat'&&callLive()&&S.draft==='Сохранить мой черновик'&&!!document.querySelector('.call-minibar')")
 ev('openChat(1)');check('Mini banner remains bound to call peer',"document.querySelector('.call-minibar strong').textContent.includes('Анна')&&S.peer===1&&C.call.peer===0")
 ev("go('call');endCall('completed')");ev("endCall('completed');connectCall();transitionCall('active')")
 check('Terminal state ignores duplicate end and late answer',"C.call.stage==='ended'&&C.history.filter(h=>h.id===C.call.id).length===1")
 ev("callToChat(0)");check('New call event follows older messages',"[...document.querySelectorAll('#chatlog > *')].at(-1).classList.contains('call-event')")
 reset();ev("demoCall('active');startCall(1)");check('Second outgoing requires explicit switch',"C.call.peer===0&&C.call.stage==='active'&&!!document.querySelector('[role=dialog]')")
 reset();ev("S.network='off';startCall(0)");check('Offline start is not queued',"C.call===null&&C.history.length===4")
 reset();ev("C.transport='blocked';startCall(0)");check('Text-only network has explicit media gate',"C.call===null&&document.querySelector('[role=dialog]').textContent.includes('канал для аудио')")
 reset();ev("S.chatState='mismatch';startCall(0)");check('Changed key blocks call',"C.call===null")
 reset();ev("S.chatState='requested';startCall(0)");check('Unaccepted contact blocks call',"C.call===null")
 reset();ev("demoCall('active');applyContactBlock();openChat(0);startCall(0)");check('Blocking ends and prevents further call',"C.call.stage==='ended'&&C.call.reason==='contactBlocked'&&peerData[0].blocked")
 reset();ev("C.permissions.mic='ask';startCall(0)");check('Permission precedes outgoing invitation',"C.call===null&&C.pendingAction.type==='start'")
 ev("grantCallPermission('mic')");check('Granted microphone starts pending outgoing',"C.call.stage==='dialing'")
 reset();ev("demoCall('incoming');C.permissions.mic='ask';answerCall();endCall('missed');grantCallPermission('mic')")
 check('Late permission cannot answer expired call',"C.call.stage==='ended'&&C.call.reason==='missed'")
 reset();ev("C.permissions.mic='ask';startCall(0);receiveIncoming(1,{id:'race-invite'});grantCallPermission('mic')")
 check('Incoming during permission cannot be overwritten',"C.call.id==='race-invite'&&C.call.stage==='incoming'")
 reset();ev("C.receiveCalls=false;window.receiveResult=receiveIncoming(0,{id:'policy-1'})")
 check('Disabled incoming policy prevents ringing',"!receiveResult.accepted&&receiveResult.reason==='policy'&&!callLive()")
 reset();ev("window.receiveResult=receiveIncoming(0,{id:'expired-1',ttlMs:-1})")
 check('Expired invite does not ring',"!receiveResult.accepted&&receiveResult.reason==='expired'&&!callLive()")
 reset();ev("receiveIncoming(0,{id:'one'});window.receiveResult=receiveIncoming(0,{id:'one'})")
 check('Incoming invitations deduplicate',"!receiveResult.accepted&&receiveResult.reason==='duplicate'&&C.call.id==='one'")
 ev("window.receiveResult=receiveIncoming(1,{id:'two'})")
 check('Second incoming does not replace first',"!receiveResult.accepted&&receiveResult.reason==='busy'&&C.call.id==='one'")
 reset();ev("demoCall('incoming');silenceCall()")
 check('Silence does not accept or reject',"C.call.ringerSilenced&&C.call.stage==='incoming'&&C.call.connectedAt===null")
 reset();ev("demoCall('active');C.call.muted=true;window.originalId=C.call.id;window.connected=C.call.connectedAt;transitionCall('reconnecting');window.deadline=C.call.deadline;transitionCall('reconnecting')")
 check('Repeated network event does not extend recovery deadline',"C.call.deadline===deadline")
 ev("transitionCall('active')");check('Recovery keeps session, clock and mute',"C.call.id===originalId&&C.call.connectedAt===connected&&C.call.muted")
 ev("transitionCall('reconnecting');C.call.deadline=performance.now()-1");page.wait_for_timeout(600)
 check('Recovery timeout ends without redial',"C.call.stage==='ended'&&C.call.reason==='networkLost'")
 reset();ev("demoCall('active');openAudioRoutes();selectAudioRoute('speaker')")
 check('Audio route pending retains previous selection',"C.call.route==='earpiece'&&C.call.routePending==='speaker'")
 page.wait_for_timeout(750);check('Route switches after simulated confirmation',"C.call.route==='speaker'&&!C.call.routePending")
 ev("C.routeFailure=true;openAudioRoutes();selectAudioRoute('bluetooth')");page.wait_for_timeout(750)
 check('Route failure preserves previous selection',"C.call.route==='speaker'&&!C.call.routePending")
 ev("closeModal();C.call.route='bluetooth';C.call.muted=true;loseHeadset()")
 check('Headset loss uses earpiece, preserves mute',"C.call.route==='earpiece'&&C.call.muted")
 reset();ev("demoCall('active');openAudioRoutes();selectAudioRoute('speaker');endCall('completed')");page.wait_for_timeout(750)
 check('Late route result does not resurrect dialog',"C.call.stage==='ended'&&!document.querySelector('#modal-overlay')")
 reset();ev("demoCall('active')");page.locator('[data-cfocus=mute]').focus();page.locator('[data-cfocus=mute]').click()
 check('Mute preserves keyboard focus',"document.activeElement.dataset.cfocus==='mute'")
 page.locator('[data-cfocus=route]').click();page.keyboard.press('Escape')
 check('Escape closes sheet, keeps call, restores focus',"!document.querySelector('#modal-overlay')&&callLive()&&document.activeElement.dataset.cfocus==='route'")
 page.keyboard.press('Escape');check('Escape minimizes rather than hangs up',"S.screen==='chat'&&callLive()")
 reset();ev("demoCall('active',{media:'video'});callBack()")
 check('Leaving video pauses local camera but preserves audio',"!C.call.localCamera&&callLive()&&S.screen==='chat'")
 reset();ev("demoCall('incoming',{media:'video'});answerCall(false);transitionCall('active')")
 check('Incoming video can be answered audio-only',"C.call.media==='audio'&&!C.call.localCamera&&C.call.stage==='active'")
 reset();ev("demoCall('active');C.videoEnabled=true;C.call.videoRequest=true;resolveVideoRequest(true)")
 check('Remote video consent never turns on local camera',"C.call.media==='video'&&!C.call.localCamera")
 reset();ev("demoCall('active');C.videoEnabled=true;C.call.videoRequest=true;resolveVideoRequest(false)")
 check('Video rejection leaves audio alive',"C.call.media==='audio'&&C.call.stage==='active'&&!C.call.videoRequest")
 reset();ev("C.videoEnabled=true;C.previewPeer=0;go('video-preview')")
 check('Video dialing requires explicit preview',"!C.cameraPreview&&!!document.querySelector('#app button[disabled]')")
 reset();ev("systemScenario('locked')")
 check('Private locked notification omits name and ID',"!document.querySelector('.call-notification').textContent.includes('Анна')&&!document.querySelector('.call-notification').textContent.includes('7K3M')")
 reset();ev("systemScenario('stale');answerCall()")
 check('Stale notification cannot start call',"C.call.stage==='ended'&&!callLive()")
 reset();ev("demoCall('active');C.history=[];render()")
 check('Clearing history does not end active session',"callLive()&&C.history.length===0")
 reset();ev("go('chat');S.draft='🙂'.repeat(1025);render();sendMessage()")
 check('Existing UTF-8 limit preserved',"S.sent.length===0&&document.getElementById('send-button').disabled")
 # Additional actual UI flows and dialog geometry.
 reset();ev("go('chat')");page.locator('#composer-text').fill('Черновик Анне')
 ev("startCall(0);connectCall();transitionCall('active');callBack();openChat(1)")
 check('Drafts are scoped to each contact',"S.draft===''&&document.getElementById('composer-text').value===''")
 page.locator('#composer-text').fill('Черновик Михаилу');ev('callBack()')
 check('Return to caller restores only caller draft',"S.peer===0&&document.getElementById('composer-text').value==='Черновик Анне'")
 ev('openChat(1)');check('Other contact draft survives call navigation',"document.getElementById('composer-text').value==='Черновик Михаилу'")
 reset();ev("demoCall('incoming');S.draft='Исходный текст';go('chat')");page.locator('#composer-text').fill('Исходный текст');ev("go('call');declineAndWrite()")
 page.get_by_role('button',name='Не могу говорить. Напишу позже.',exact=True).click()
 check('Decline and write preserves draft without sending',"S.draft.includes('Исходный текст')&&S.draft.includes('Не могу говорить')&&S.sent.length===0&&C.call.stage==='ended'")
 reset();ev("go('chat');C.autoAnswer=true;startCall(0)");page.wait_for_timeout(4600)
 check('Default interactive demo completes connection automatically',"C.call.stage==='active'&&C.call.connectedAt!==null")
 reset();ev("demoCall('active')");page.locator('[data-cfocus=route]').click()
 ev("selectAudioRoute('speaker')");page.wait_for_timeout(750)
 check('Endpoint change returns focus to stable route control',"document.activeElement.dataset.cfocus==='route'")
 reset();ev("demoCall('active');openAudioRoutes()")
 page.locator('#modal-overlay button').last.focus();page.keyboard.press('Tab')
 check('Modal keyboard focus wraps inside route sheet',"document.activeElement===document.querySelector('#modal-overlay button')")
 page.keyboard.press('Shift+Tab')
 check('Modal reverse focus wraps to last control',"document.activeElement===[...document.querySelectorAll('#modal-overlay button')].at(-1)")
 # Every modal case at 360 CSS pixels / 200% text.
 for script in ["demoCall('active');openAudioRoutes()", "scenarioPermission()", "go('call-permissions');permissionStateDialog('mic')", "go('calls');callContactPicker()", "go('call-settings');clearCallHistory()"]:
  reset();ev("document.documentElement.style.setProperty('--scale','2');document.getElementById('live-device').style.width='360px';"+script)
  check('Dialog geometry: '+script,"""(()=>{const app=document.getElementById('app').getBoundingClientRect();return [...document.querySelectorAll('#modal-overlay button')].every(b=>{const r=b.getBoundingClientRect();return r.width>=48&&r.height>=48&&r.left>=app.left-1&&r.right<=app.right+1})})()""")
 # Mobile at 360px, large font: controls remain within viewport.
 page.set_viewport_size({'width':360,'height':800});reset();ev("document.documentElement.style.setProperty('--scale','2');demoCall('active',{media:'video'})")
 page.screenshot(path=str(ROOT/'mobile-calls-360-large-text.png'))
 check('Mobile document has no horizontal overflow',"document.documentElement.scrollWidth<=360")
 (ROOT/'qa-results.json').write_text(json.dumps(results,ensure_ascii=False,indent=2))
 bad=[g for g in results['geometry'] if g['overflow'] or g['small'] or g['rounded'] or g['horizontal']]
 print('GEOMETRY',len(results['geometry']),'FAIL',len(bad))
 print(json.dumps(bad[:20],ensure_ascii=False,indent=2))
 print('FUNCTIONAL',len(results['checks']),'FAIL',sum(not x['ok'] for x in results['checks']))
 print('JS_ERRORS',results['errors'],'NETWORK_REQUESTS',results['requests'])
 b.close()
 if bad or any(not x['ok'] for x in results['checks']) or results['errors'] or results['requests']:
  raise SystemExit(1)
