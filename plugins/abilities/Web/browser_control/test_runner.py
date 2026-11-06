"""Protocol boundaries use mock WebDriver; optional real Chromium local fixture."""
import asyncio
import base64
import importlib.util
import json
import os
import tempfile
import threading
import time
import unittest
import uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
from unittest import mock

spec=importlib.util.spec_from_file_location("v3_admin_browser",Path(__file__).with_name("adapter.py"))
browser=importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser)
PNG=base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ4cAAAAASUVORK5CYII=")
class Element:
    def __init__(self,driver,selector):self.driver=driver;self.selector=selector
    @property
    def text(self):return json.dumps({"user_id":self.driver.identity,"csrf_token":self.driver.csrf})
    def is_displayed(self):return True
    def is_enabled(self):return True
    def click(self):
        self.driver.clicks+=1;self.driver.revision+=1
        if self.driver.escape_on_click:self.driver.current_url="https://outside.invalid/"
    def send_keys(self,text):self.driver.typed.append(text);self.driver.revision+=1
CREDENTIAL_FIELDS=("#password","#user","#code")
class Driver:
    def __init__(self):self.current_url="";self.identity="admin";self.csrf=str(uuid.uuid4());self.cookies=[];self.revision=0;self.jar=[];self.cdp_log=[]
    clicks=0
    ready="ready"
    escape_on_click=False
    closed=False
    failure=None
    signin=False
    def get(self,url):self.current_url=url;self.revision+=1
    def find_elements(self,by,selector):return [Element(self,selector)]
    def execute_cdp_cmd(self,command,params):
        self.cdp_log.append(command)
        if command=="Network.getAllCookies":return {"cookies":list(self.jar)}
        if command=="Network.setCookies":self.jar.extend(params["cookies"])
        if command=="Network.clearBrowserCookies":self.jar.clear()
        return {}
    def add_cookie(self,cookie):self.cookies.append(cookie)
    def find_element(self,by,selector):
        if self.failure:raise RuntimeError(self.failure)
        return Element(self,selector)
    def execute_script(self,script,*args):
        if "V3_OBSERVE" in script:return self.revision
        if "V3_BOOT" in script:return self.ready
        if "V3_READ" in script:return {"text":"Visible bounded text "+args[1]+" "+self.csrf,"missing":False,"truncated":False}
        if "V3_TARGET" in script:
            return {"allowed":True,"secret":args[0]=="#password","credential":args[0] in CREDENTIAL_FIELDS,"href":
                "https://outside.invalid/" if args[0]=="#external" else "","tag":"input"}
        if "V3_SIGNIN" in script:return self.signin() if callable(self.signin) else self.signin
        if "V3_CREDENTIAL" in script:return args[0].selector in CREDENTIAL_FIELDS
        if "V3_CHECK" in script:return True
        if "V3_MASK" in script:self.masked_secrets=args;self.revision+=1;return 1
        if "V3_UNMASK" in script:self.revision+=1;return None
        raise AssertionError("Unexpected native script")
    def get_screenshot_as_png(self):return PNG
    def quit(self):self.closed=True
class BrowserTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.sid=str(uuid.uuid4());self.base=self.root/"workspace"/self.sid;self.base.mkdir(parents=True)
        self.trusted={"session_id":self.sid,"user_id":"admin","agent_id":"shared_default",
            "cookie":str(uuid.uuid4()),"generation":"first","expires_at":time.time()+300,
            "allowed_session_ids":[self.sid]}
        self.drivers=[]
        def factory(profile,origin,**options):
            driver=Driver();driver.typed=[];self.drivers.append(driver);return driver
        self.service=browser.BrowserService(self.root/"private",driver_factory=factory)
    async def asyncTearDown(self):
        await self.service.close_all(self.base)
        self.temp.cleanup()
    async def call(self,name=None,**args):
        # Input actions belong to browser_input; signed-in routes are passed as url.
        if name is None:name="browser_input" if args.get("action") in {"click","type","press","save_login"} else "browser_read"
        if "path" in args and name=="browser_read":args["url"]=args.pop("path")
        return await self.service.call(name,args,self.base,self.trusted)
    async def test_optional_action_schema_defaults_preserve_boundaries(self):
        self.assertEqual((await self.call(action="navigate",url="/app"))["status"],"success")
        for action in ("read","screenshot","close"):
            result=await self.call(action=action,path="",selector="",text="")
            self.assertEqual(result["status"],"success",(action,result))
        result=await self.call(action="navigate",path="/api/v3/auth/me",selector="",text=None)
        self.assertEqual(result["status"],"success",result)
        for args in [{"action":"screenshot","path":"/app","selector":"","text":""},
                     {"action":"read","path":"","selector":"","text":"substantive"},
                     {"action":"click","path":"","selector":"","text":""},
                     {"action":"navigate","path":"","selector":"","text":""},
                     {"action":"close","unknown":""}]:
            self.assertEqual((await self.call(**args))["status"],"error",args)
        args={"action":"read","selector":None,"url":None,"text":None}
        self.assertEqual(browser.validate_args("browser_read",args),"read")
        self.assertEqual(browser.validate_args("browser_read",{"action":"read","expected_sequence":0}),"read")
        with self.assertRaises(browser.Rejected):
            browser.validate_args("browser_read",{"action":"read","expected_sequence":1})
        self.assertEqual(args,{"action":"read"})
    def test_native_read_script_preserves_javascript_newline_escape(self):
        self.assertIn("text+=chunk+'\\n';",browser.READ_SCRIPT)
        self.assertNotIn("text+=chunk+'\n';",browser.READ_SCRIPT)
    async def test_identity_csrf_is_masked_in_read_and_screenshot(self):
        await self.call(action="navigate",url="/api/v3/auth/me")
        csrf=self.drivers[0].csrf
        read=await self.call(action="read")
        self.assertNotIn(csrf,read["text"])
        self.assertNotIn(self.trusted["cookie"],read["text"])
        shot=await self.call(action="screenshot")
        self.assertEqual(shot["status"],"success")
        self.assertEqual(self.drivers[0].masked_secrets,(self.trusted["cookie"],csrf))
    async def test_login_private_cookie_fixed_origin_and_current_routes(self):
        result=await self.call(action="navigate",url="/app?tab=agents&agent=shared_default&agentView=abilities")
        self.assertEqual(result["status"],"success")
        self.assertNotIn(self.trusted["cookie"],json.dumps(result))
        self.assertTrue(result["ready"])
        self.assertEqual(self.drivers[0].cookies[0]["name"],"v3_session")
        self.assertTrue(self.drivers[0].cookies[0]["httpOnly"])
        self.assertTrue(self.drivers[0].cookies[0]["secure"])
        self.assertNotIn(self.base,self.service.states[self.sid].directory.parents)
        for path in ["https://outside.invalid/","//outside.invalid/","file:///etc/passwd",
                     "/app?session_id="+str(uuid.uuid4()),"/app?tab=agents&agent=another",
                     "/api/v3/sessions/"+str(uuid.uuid4()),"/app?tab=account&cookie=x"]:
            result=await self.call(action="navigate",path=path)
            self.assertEqual(result["status"],"error",path)
        self.assertEqual((await self.call(action="navigate",path="/api/v3/auth/me"))["status"],"success")
    async def test_current_app_routes_viewport_page_errors_and_named_rejections(self):
        origin=browser.ORIGIN
        await self.call(action="navigate",url="/app")
        for path in ["/app?tab=app","/app?tab=builds&job_id=job-1","/app?tab=agents&agent=shared_default&agentView=identity&agentScope=sessions",
                     "/app?tab=app&appView=files&path=/notes/today.md","/app?tab=session-metrics"]:
            self.assertEqual((await self.call(action="navigate",path=path))["status"],"success",path)
        for path,reason in [("/app?tab=app&chat_invite=x","Unsupported app route parameters; allowed: agent, agentScope"),("/app?tab=app&embed_auth=x","Unsupported app route parameters"),
                            ("/app?tab=Agents%20x","Unsupported development page"),("/app?agent=another","Another agent is unavailable")]:
            result=await self.call(action="navigate",path=path)
            self.assertTrue(result["output"].startswith("Private browser request rejected: "+reason),path)
        # A page that drifted outside the routes blocks reads but navigation recovers.
        driver=self.drivers[0];driver.current_url=origin+"/app?tab=agents&agent=another"
        self.assertEqual((await self.call(action="read"))["output"],"Private browser request rejected: Another agent is unavailable")
        self.assertEqual((await self.call(action="navigate",path="/app?tab=app"))["status"],"success")
        driver.cdp=[]
        driver.execute_cdp_cmd=lambda command,params:driver.cdp.append((command,params))
        phone=await self.call(action="viewport",preset="phone")
        self.assertEqual((phone["width"],phone["height"],phone["navigate_required"]),(390,844,True))
        self.assertEqual(driver.cdp,[("Emulation.setDeviceMetricsOverride",{"width":390,"height":844,"deviceScaleFactor":1,"mobile":True}),
            ("Emulation.setTouchEmulationEnabled",{"enabled":True})])
        self.assertEqual((await self.call(action="viewport",preset="desktop"))["width"],1280)
        self.assertIn("one of phone",(await self.call(action="viewport",preset="watch"))["output"])
        self.assertEqual((await self.call(action="viewport",preset="tablet"))["width"],768)
        self.assertEqual(driver.cdp[-1],("Emulation.setTouchEmulationEnabled",{"enabled":True}))
        # The preset is re-applied after navigation and before a screenshot.
        driver.cdp.clear()
        await self.call(action="navigate",path="/app?tab=app")
        self.assertEqual(driver.cdp[0][1]["width"],768)
        driver.cdp.clear();await self.call(action="screenshot")
        self.assertEqual(driver.cdp[0][1]["width"],768)
        self.assertEqual((await self.call(action="viewport"))["status"],"error")
        driver.get_log=lambda kind:[{"level":"SEVERE","message":"app.js 455 Uncaught TypeError "+self.trusted["cookie"]},
            {"level":"INFO","message":"ignored"},{"level":"SEVERE","message":"x"*400}]
        errors=(await self.call(action="read"))["page_errors"]
        self.assertEqual(errors[0],"app.js 455 Uncaught TypeError [redacted]");self.assertEqual(len(errors),2);self.assertEqual(len(errors[1]),300)
        self.assertNotIn(self.trusted["cookie"],json.dumps(errors))
        self.assertIn("whole number",(await self.call(action="read",offset=-1))["output"])
        self.assertEqual((await self.call(action="read",offset=12000))["status"],"success")
        clicked=await self.call(action="click",selector="#button")
        self.assertEqual(clicked["target"],{"tag":"input","label":""})
    async def test_measure_wait_animations_scroll_press_and_emulate(self):
        import types
        await self.call(action="navigate",url="/app")
        driver=self.drivers[0];original=driver.execute_script;polls=[];focus={"tag":"button","id":"next","label":"Next","secret":False};measured_args=[]
        cookie=self.trusted["cookie"]
        def script(source,*args):
            if "V3_MEASURE" in source:
                measured_args.append(args)
                return {"checks":{"viewport":{"width":390,"height":844},"page_overflow_x":0,"matches":1,"rendered":1,
                    "under_44px":{"count":1,"smallest":[{"label":"Save "+cookie,"width":40,"height":32}]},"outside_viewport":0,
                    "content_overflowing":{"count":0,"first":[]}},
                    "elements":[{"tag":"button","label":"Save "+cookie,"box":{"x":0,"y":0,"width":40,"height":32}}],"next_offset":None}
            if "V3_CENTER" in source:return None if args[0]=="#none" else {"x":20,"y":16}
            if "V3_PERFORMANCE" in source:return {"time_to_first_byte_ms":80,"first_contentful_paint_ms":420}
            if "V3_ANIMATIONS" in source:return {"missing":False,"count":1,"animations":[{"type":"CSSTransition","name":"opacity","duration_ms":200}]}
            if "V3_CHECK" in source:polls.append(args);return len(polls)>=3
            if "V3_SCROLL" in source:return {"missing":args[0]=="#none","scroll_y":args[2]}
            if "V3_FOCUS" in source:return focus
            return original(source,*args)
        driver.execute_script=script
        driver.cdp=[];driver.execute_cdp_cmd=lambda command,params:driver.cdp.append((command,params))
        driver.switch_to=types.SimpleNamespace(active_element=Element(driver,"active"))
        self.assertIn("requires a selector",(await self.call(action="measure"))["output"])
        measured=await self.call(action="measure",selector="button")
        self.assertEqual(measured["elements"][0]["box"]["height"],32)
        self.assertEqual(measured["elements"][0]["label"],"Save [redacted]")
        self.assertEqual(measured["checks"]["under_44px"]["smallest"][0]["label"],"Save [redacted]")
        self.assertEqual(measured_args[-1],("button",20,0,False))
        await self.call(action="measure",selector="button",offset=20,styles=True)
        self.assertEqual(measured_args[-1],("button",5,20,True))
        self.assertIn("styles must be",(await self.call(action="measure",selector="button",styles="yes"))["output"])
        self.assertEqual((await self.call(action="performance"))["first_contentful_paint_ms"],420)
        # Pixels: one 1x1 capture per point, decoded from the PNG, under the screenshot masks.
        import struct as _struct,zlib as _zlib
        def chunk(kind,body):return _struct.pack(">I",len(body))+kind+body+_struct.pack(">I",_zlib.crc32(kind+body))
        one=b"\x89PNG\r\n\x1a\n"+chunk(b"IHDR",_struct.pack(">IIBBBBB",1,1,8,6,0,0,0))+chunk(b"IDAT",_zlib.compress(b"\x04\x12\x34\x56\xff"))+chunk(b"IEND",b"")
        captured=[]
        def cdp(command,params):
            captured.append((command,params));driver.cdp.append((command,params))
            return {"data":base64.b64encode(one).decode()} if command=="Page.captureScreenshot" else {}
        driver.execute_cdp_cmd=cdp
        shot=await self.call(action="pixels",points=[{"x":5,"y":7},{"x":0,"y":0}])
        self.assertEqual([p["color"] for p in shot["pixels"]],["#123456","#123456"])
        self.assertEqual(captured[0][1]["clip"],{"x":5,"y":7,"width":1,"height":1,"scale":1})
        self.assertEqual(driver.masked_secrets,(cookie,driver.csrf))
        self.assertEqual((await self.call(action="pixels",selector="button"))["pixels"][0]["x"],20)
        self.assertIn("No rendered element",(await self.call(action="pixels",selector="#none"))["output"])
        self.assertIn("points must be",(await self.call(action="pixels",points=[{"x":-1,"y":0}]))["output"])
        self.assertIn("points or a selector",(await self.call(action="pixels"))["output"])
        self.assertEqual((await self.call(action="animations"))["animations"][0]["duration_ms"],200)
        self.assertIn("requires a selector",(await self.call(action="wait_for",condition="visible"))["output"])
        self.assertIn("timeout_ms",(await self.call(action="wait_for",condition="quiet",timeout_ms=99999))["output"])
        self.assertIn("one of",(await self.call(action="wait_for",condition="forever"))["output"])
        waited=await self.call(action="wait_for",condition="quiet",quiet_ms=200)
        self.assertEqual((waited["met"],waited["last_action"],len(polls)),(True,"navigate",3))
        self.assertIsInstance(waited["since_last_action_ms"],int)
        self.assertEqual(polls[0],("quiet","","",200))
        self.assertIn("does not exist",(await self.call(action="scroll",selector="#none"))["output"])
        self.assertIn("selector to bring into view",(await self.call(action="scroll"))["output"])
        self.assertEqual((await self.call(action="scroll",dy=600))["scroll_y"],600)
        self.assertIn("Read the current page",(await self.call(action="press",key="Tab"))["output"])
        await self.call(action="read")
        self.assertIn("Key must be one of",(await self.call(action="press",key="a"))["output"])
        pressed=await self.call(action="press",key="Tab")
        self.assertEqual((pressed["applied"],pressed["focused"]["label"]),(True,"Next"))
        self.assertEqual(driver.typed[-1],"\ue004")
        self.assertIn("Read the current page",(await self.call(action="press",key="Tab"))["output"])
        # A key types nothing, so Enter may submit a password field; its label stays private.
        focus["secret"]=True;focus["label"]="secret label";await self.call(action="read")
        pressed=await self.call(action="press",key="Enter")
        self.assertEqual((pressed["applied"],pressed["target"]["label"]),(True,""))
        self.assertIn("color_scheme must be",(await self.call(action="emulate",color_scheme="sepia"))["output"])
        self.assertIn("needs color_scheme",(await self.call(action="emulate"))["output"])
        emulated=await self.call(action="emulate",color_scheme="dark",reduced_motion="reduce")
        self.assertEqual((emulated["color_scheme"],emulated["reduced_motion"]),("dark","reduce"))
        self.assertEqual(driver.cdp[-1],("Emulation.setEmulatedMedia",{"features":[{"name":"prefers-color-scheme","value":"dark"},
            {"name":"prefers-reduced-motion","value":"reduce"}]}))
        # Media emulation is re-applied after navigation, like the viewport.
        driver.cdp.clear();await self.call(action="navigate",path="/app?tab=app")
        self.assertEqual(driver.cdp[-1][0],"Emulation.setEmulatedMedia")
    async def test_signed_out_public_urls_site_policy_guard_and_tool_split(self):
        public={k:v for k,v in self.trusted.items() if k!="cookie"}
        public["sites"]={"allow":[],"block":["blocked-by-policy.test"]}
        for url,reason in [("http://localhost/","Private and internal"),("https://10.0.0.1/","Private and internal"),
                           ("https://"+"name"+":"+"value"+"@example.com/","user name or password"),("ftp://example.com/","full http"),
                           ("https://example.com:8080/","ports 80 and 443"),("/app","full http"),("https://sub.blocked-by-policy.test/","blocks")]:
            with self.assertRaises(browser.Rejected) as caught:browser.validate_url(url,public)
            self.assertIn(reason,str(caught.exception),url)
        allow={**public,"sites":{"allow":["example.com"],"block":[]}}
        self.assertEqual(browser.validate_url("https://www.example.com/a?b=1",allow),"https://www.example.com/a?b=1")
        with self.assertRaises(browser.Rejected) as caught:browser.validate_url("https://other.test/",allow)
        self.assertIn("may only open: example.com",str(caught.exception))
        with self.assertRaises(browser.Rejected):browser.validate_trusted(self.base,{**public,"sites":{"allow":["Bad Host"],"block":[]}})
        options=[]
        def factory(profile,origin,**kwargs):
            options.append((origin,kwargs));driver=Driver();driver.typed=[];self.drivers.append(driver)
            def get(url):
                driver.revision+=1
                driver.current_url="chrome-error://chromewebdata/" if "refused.test" in url else url
            driver.get=get;return driver
        self.service.factory=factory
        call=lambda name,**args:self.service.call(name,args,self.base,public)
        opened=await call("browser_read",action="navigate",url="https://example.com/")
        self.assertEqual((opened["status"],opened["signed_in"],opened["url"]),("success",False,"https://example.com/"),opened)
        self.assertIsNone(options[0][0]);self.assertEqual(options[0][1]["proxy"],browser.egress_guard().port)
        self.assertEqual(self.drivers[-1].cookies,[])
        browser.egress_guard().refused("refused.test","it resolves to a private or internal address")
        blocked=await call("browser_read",action="navigate",url="https://refused.test/")
        self.assertIn("Blocked: refused.test",blocked["output"])
        self.assertIn("navigate to another URL",(await call("browser_read",action="read"))["output"])
        self.assertEqual((await call("browser_read",action="navigate",url="https://example.com/next"))["status"],"success")
        read=await call("browser_read",action="read")
        self.assertEqual(read["status"],"success");self.assertTrue(read["text"].startswith("Visible bounded text"))
        self.assertIn("browser_input",(await call("browser_read",action="click",selector="#button"))["output"])
        self.assertIn("browser_read",(await call("browser_input",action="read"))["output"])
        self.assertEqual((await call("browser_input",action="click",selector="#button"))["status"],"success")
        self.assertIn("signed_in must be",(await call("browser_read",action="navigate",url="https://example.com/",signed_in="yes"))["output"])
        # Signing in replaces the signed-out browser with one locked to the development origin.
        self.service.factory=lambda profile,origin,**kwargs:(self.drivers.append(Driver()) or self.drivers[-1])
        self.drivers.append(None);self.drivers.pop()
        signed=await self.call(action="navigate",url="/app")
        self.assertEqual(signed["status"],"success",signed);self.assertTrue(signed["signed_in"])
        self.assertIn("Only exact relative",(await self.call(action="navigate",url="https://example.com/"))["output"])
    async def test_fresh_read_before_each_input_and_secret_targets_blocked(self):
        self.assertEqual((await self.call(action="click",selector="#button"))["status"],"error")
        read=await self.call(action="read")
        self.assertEqual(read["status"],"success")
        self.assertNotIn(self.trusted["cookie"],read["text"])
        self.assertTrue(read["fresh_for_input"])
        self.assertEqual((await self.call(action="click",selector="#button"))["status"],"success")
        self.assertEqual((await self.call(action="type",selector="#text",text="x"))["status"],"error")
        # Unrelated page changes after a read (live timestamps) no longer block input.
        await self.call(action="read")
        self.drivers[0].revision+=1
        self.assertEqual((await self.call(action="click",selector="#button"))["status"],"success")
        await self.call(action="read")
        self.assertEqual((await self.call(action="type",selector="#password",text="secret"))["status"],"error")
        await self.call(action="read")
        self.assertEqual((await self.call(action="click",selector="#external"))["status"],"error")
        await self.call(action="read")
        self.assertEqual((await self.call(action="type",selector="#text",text="literal"))["status"],"success")
        self.assertEqual(self.drivers[0].typed,["literal"])
    async def test_input_escape_is_uncertain_not_replayed_and_closes_profile(self):
        await self.call(action="read")
        self.drivers[0].escape_on_click=True
        result=await self.call(action="click",selector="#button")
        self.assertEqual(result["status"],"uncertain")
        self.assertEqual(self.drivers[0].clicks,1)
        self.assertTrue(self.drivers[0].closed)
        self.assertNotIn(self.sid,self.service.states)
    async def test_artifacts_are_real_private_pngs_owner_checked_and_bounded(self):
        screenshot=await self.call(action="screenshot")
        self.assertEqual(screenshot["status"],"success")
        self.assertEqual(screenshot["media_type"],"image/png")
        self.assertEqual(screenshot["width"],1)
        self.assertNotIn(str(self.root),json.dumps(screenshot))
        self.assertEqual(self.service.read_artifact(self.base,self.trusted,screenshot["artifact_id"]),PNG)
        changed=dict(self.trusted,user_id="other")
        with self.assertRaises(browser.Rejected):
            self.service.read_artifact(self.base,changed,screenshot["artifact_id"])
        with self.assertRaises(browser.Rejected):
            self.service.read_artifact(self.base,self.trusted,"../secret")
        with mock.patch.object(browser,"MAX_PNG",24):
            result=await self.call(action="screenshot")
            self.assertEqual(result["status"],"error")
    async def test_full_screenshot_folder_drops_the_oldest(self):
        with mock.patch.object(browser,"MAX_ARTIFACTS",3):
            shots=[]
            for _ in range(5):
                shot=await self.call(action="screenshot");self.assertEqual(shot["status"],"success",shot);shots.append(shot["artifact_id"])
                time.sleep(0.01)
        folder=self.service.states[self.sid].directory/"artifacts"
        self.assertEqual(sorted(p.stem for p in folder.glob("*.json")),sorted(shots[2:]))
        self.assertEqual(len(list(folder.glob("*.png"))),3)
        with self.assertRaises(OSError):self.service.read_artifact(self.base,self.trusted,shots[0])
        self.assertEqual(self.service.read_artifact(self.base,self.trusted,shots[-1]),PNG)
    def signed_out(self,**extra):
        public={k:v for k,v in self.trusted.items() if k!="cookie"}
        return {**public,"sites":{"allow":[],"block":[]},**extra}
    def test_totp_codes_match_the_rfc_vector(self):
        self.assertEqual(browser.totp_code("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",at=59),"287082")
        self.assertEqual(browser.totp_code("gezd gnbv-gy3t qojq gezd gnbv gy3t qojq",at=59),"287082")
        with self.assertRaises(browser.Rejected):browser.totp_code("not base32!")
        self.assertEqual(browser.origin_of("https://Example.com:443/a?b"),"https://example.com")
        self.assertTrue(browser.valid_origin("https://example.com"));self.assertFalse(browser.valid_origin("http://example.com"))
    async def test_vault_values_go_only_into_sign_in_fields_on_their_site_and_never_come_back(self):
        value="Pa55-"+"PRIVATE";secrets={"site_password":{"value":value,"origin":"https://example.com"},
            "site_totp":{"value":"GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ","origin":"https://example.com"}}
        trusted=self.signed_out(secrets=secrets)
        call=lambda name,**args:self.service.call(name,args,self.base,trusted)
        self.assertEqual((await call("browser_read",action="navigate",url="https://example.com/login"))["status"],"success")
        self.assertIn("literal text or a vault",(await call("browser_input",action="type",selector="#password",text="x",vault="site_password"))["output"])
        self.assertIn("goes with a vault",(await call("browser_input",action="type",selector="#user",text="me",totp=True))["output"])
        await call("browser_read",action="read")
        self.assertIn("sign-in field",(await call("browser_input",action="type",selector="#search",vault="site_password"))["output"])
        await call("browser_read",action="read")
        self.assertIn("unavailable",(await call("browser_input",action="type",selector="#password",vault="missing"))["output"])
        await call("browser_read",action="read")
        typed=await call("browser_input",action="type",selector="#password",vault="site_password")
        self.assertEqual((typed["status"],typed["vault"]),("success","site_password"),typed)
        driver=self.drivers[-1];self.assertEqual(driver.typed[-1],value)
        await call("browser_read",action="read")
        code=await call("browser_input",action="type",selector="#code",vault="site_totp",totp=True)
        self.assertEqual(code["status"],"success");self.assertRegex(driver.typed[-1],r"^\d{6}$")
        # A page that echoes the value anywhere has it scrubbed, in plain and URL-encoded form.
        driver.get_log=lambda kind:[{"level":"SEVERE","message":"bad login for "+value+" / Pa55-PRIVATE"}]
        read=await call("browser_read",action="read")
        self.assertNotIn(value,json.dumps(read));self.assertIn("[redacted]",read["page_errors"][0])
        # Another site never receives it.
        await call("browser_read",action="navigate",url="https://other.example.org/login");await call("browser_read",action="read")
        self.assertIn("only for https://example.com",(await call("browser_input",action="type",selector="#password",vault="site_password"))["output"])
        with self.assertRaises(browser.Rejected):
            browser.validate_trusted(self.base,self.signed_out(secrets={"x":{"value":"v","origin":"http://example.com"}}))
    async def test_saved_sign_in_is_kept_restored_and_signs_in_again(self):
        value="Pa55-"+"PRIVATE";secrets={"site_password":{"value":value,"origin":"https://example.com"}}
        trusted=self.signed_out(secrets=secrets)
        call=lambda name,**args:self.service.call(name,args,self.base,trusted)
        await call("browser_read",action="navigate",url="https://example.com/login");await call("browser_read",action="read")
        await call("browser_input",action="type",selector="#user",text="me@example.com");await call("browser_read",action="read")
        await call("browser_input",action="type",selector="#password",vault="site_password");await call("browser_read",action="read")
        await call("browser_input",action="click",selector="#submit")
        driver=self.drivers[-1]
        driver.jar=[{"name":"sid","value":"abc","domain":".example.com","path":"/","secure":True,"session":True,"expires":-1,"sameSite":"Lax"},
            {"name":"tracker","value":"t","domain":"ads.test","path":"/"}]
        self.assertIn("not on https://example.org",(await call("browser_input",action="save_login",origin="https://example.org"))["output"])
        saved=await call("browser_input",action="save_login",origin="https://example.com")
        self.assertEqual((saved["status"],saved["cookies"],saved["steps"]),("success",1,3),saved)
        login=saved["site_login"]
        self.assertEqual(login["cookies"],[{"name":"sid","value":"abc","domain":".example.com","path":"/","secure":True,"sameSite":"Lax"}])
        self.assertEqual(login["steps"][1],{"action":"type","selector":"#password","vault":"site_password","totp":False})
        self.assertEqual(login["login_url"],"https://example.com/login");self.assertNotIn(value,json.dumps(saved))
        # Opening another site clears the cookies; opening the saved site loads its own.
        await call("browser_read",action="navigate",url="https://other.example.org/");self.assertEqual(driver.jar,[])
        restored=await self.service.call("browser_read",{"action":"navigate","url":"https://example.com/account"},self.base,{**trusted,"site":login})
        self.assertEqual((restored["signed_in"],restored["site_login"]),(True,"restored"),restored)
        self.assertEqual(driver.jar,login["cookies"])
        # Signed out by the site: the saved steps sign in again, and the new cookies come back for Rust to keep.
        await call("browser_read",action="navigate",url="https://other.example.org/")
        driver.typed.clear();driver.signin=lambda:value not in driver.typed
        again=await self.service.call("browser_read",{"action":"navigate","url":"https://example.com/account"},self.base,{**trusted,"site":login})
        self.assertEqual((again["status"],again["site_login"],again["url"]),("success","signed_in_again","https://example.com/account"),again)
        self.assertEqual(driver.typed,["me@example.com",value]);self.assertIn("site_cookies",again);self.assertNotIn(value,json.dumps(again))
        await call("browser_read",action="navigate",url="https://other.example.org/")
        driver.signin=True
        failed=await self.service.call("browser_read",{"action":"navigate","url":"https://example.com/account"},self.base,{**trusted,"site":login})
        self.assertEqual(failed["error_kind"],"signin_failed",failed)
        await call("browser_read",action="navigate",url="https://other.example.org/")
        expired=await self.service.call("browser_read",{"action":"navigate","url":"https://example.com/"},self.base,{**trusted,"site":{**login,"steps":[]}})
        self.assertEqual((expired["signed_in"],expired["site_login"]),(False,"expired"))
        self.assertIn("for external sites",(await self.call(action="save_login",origin="https://example.com"))["output"])
    async def test_rotation_expiry_unknown_fields_and_errors_never_echo_cookie(self):
        await self.call(action="navigate",url="/app")
        original=self.drivers[0]
        self.trusted["cookie"]=str(uuid.uuid4());self.trusted["generation"]="second"
        self.assertEqual((await self.call(action="read"))["status"],"success")
        self.assertTrue(original.closed)
        self.drivers[-1].failure=self.trusted["cookie"]
        result=await self.call(action="click",selector="#button")
        self.assertNotIn(self.trusted["cookie"],json.dumps(result))
        self.assertEqual((await self.call(action="evaluate",js="return document.cookie"))["status"],"error")
        self.assertEqual((await self.call(action="read",user_id="foreign"))["status"],"error")
        self.trusted["expires_at"]=time.time()-1
        self.assertEqual((await self.call(action="read"))["status"],"error")
    async def test_signed_in_navigation_waits_for_boot_and_names_where_it_stopped(self):
        self.assertEqual((await self.call(action="navigate",url="/app"))["ready"],True)
        self.drivers[-1].ready="signed_out"
        result=await self.call(action="navigate",url="/app?tab=builds")
        self.assertEqual(result["error_kind"],"app_signed_out")
        self.drivers[-1].ready="booting"
        with mock.patch.object(browser,"READY_MS",300):
            result=await self.call(action="navigate",url="/app?tab=builds")
        self.assertEqual(result["error_kind"],"app_boot_stalled");self.assertIn("page_errors",result)
        # API routes are JSON, not the app, so they do not wait for boot.
        self.assertNotIn("ready",await self.call(action="navigate",url="/api/v3/auth/me"))
    async def test_crashed_tab_is_named_and_closed_and_other_failures_name_their_type(self):
        await self.call(action="navigate",url="/app")
        crashed=self.drivers[-1];crashed.execute_script=lambda *args:(_ for _ in ()).throw(RuntimeError("tab crashed"))
        result=await self.call(action="read")
        self.assertEqual(result["error_kind"],"browser_crashed")
        self.assertTrue(crashed.closed);self.assertNotIn(self.sid,self.service.states)
        await self.call(action="navigate",url="/app")
        self.drivers[-1].execute_script=lambda *args:(_ for _ in ()).throw(KeyError(self.trusted["cookie"]))
        result=await self.call(action="read")
        self.assertEqual(result["error_kind"],"browser_backend_failed");self.assertIn("(KeyError)",result["output"])
        self.assertNotIn(self.trusted["cookie"],json.dumps(result))
    async def test_recorder_has_real_bounded_frames_and_cancellation_closes(self):
        start=await self.service.record({"action":"start"},self.base,self.trusted)
        self.assertTrue(start["recording"])
        await asyncio.sleep(1.1)
        stopped=await self.service.record({"action":"stop"},self.base,self.trusted)
        self.assertFalse(stopped["recording"])
        self.assertGreaterEqual(stopped["frames"],1)
        self.assertFalse(stopped["video"])
        self.assertEqual(stopped["format"],"bounded_png_frames")
        self.assertEqual(self.service.read_artifact(self.base,self.trusted,stopped["artifacts"][0]["artifact_id"]),PNG)
        state=self.service.states[self.sid]
        await self.service.close_all(self.base)
        self.assertTrue(state.cancelled.is_set())
        self.assertTrue(state.driver.closed)
        self.assertNotIn(self.sid,self.service.states)
    async def test_identity_mismatch_and_profile_symlink_fail_closed(self):
        def wrong(profile,origin):
            driver=Driver();driver.identity="another-user";driver.typed=[];self.drivers.append(driver);return driver
        self.service.factory=wrong
        result=await self.call(action="navigate",url="/app")
        self.assertEqual(result["status"],"error")
        self.assertTrue(self.drivers[-1].closed)
        folder=self.root/"private"/self.sid/"chrome"
        if folder.exists():
            import shutil;shutil.rmtree(folder)
        folder.symlink_to(self.base,target_is_directory=True)
        self.assertEqual((await self.call(action="navigate",url="/app"))["status"],"error")
        self.assertEqual(list(self.base.iterdir()),[])

@unittest.skipUnless(os.environ.get("V3_BROWSER_REAL_TEST")=="1","requires image Chromium/Selenium; run explicitly")
class RealBrowserTests(unittest.IsolatedAsyncioTestCase):
    async def test_real_local_fixture_identity_dom_input_and_png(self):
        sid=str(uuid.uuid4());cookie=str(uuid.uuid4())
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_GET(self):
                if self.path=="/api/v3/auth/me":
                    body=json.dumps({"user_id":"admin" if self.headers.get("Cookie")=="v3_session="+cookie else "unauthenticated"}).encode()
                    media="application/json"
                elif self.path.startswith("/app"):
                    body=b'<html><body><script>window.__boot={revealed:true}</script><p id="text">Fixture ready</p><button id="go" onclick="document.getElementById(\'text\').textContent=\'Clicked\'">Go</button><input id="password" type="password" value="PRIVATE_VALUE"></body></html>'
                    media="text/html"
                else:body=b"ready";media="text/plain"
                self.send_response(200);self.send_header("Content-Type",media);self.end_headers();self.wfile.write(body)
        server=ThreadingHTTPServer(("127.0.0.1",0),Handler)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);base=root/"workspace"/sid;base.mkdir(parents=True)
            trusted={"session_id":sid,"user_id":"admin","agent_id":"shared_default","cookie":cookie,
                "generation":"fixture","expires_at":time.time()+300,"allowed_session_ids":[sid]}
            service=browser.BrowserService(root/"private",test_origin="http://127.0.0.1:"+str(server.server_port))
            try:
                login=await service.call("browser_read",{"action":"navigate","url":"/app"},base,trusted)
                self.assertEqual(login["status"],"success",login)
                self.assertNotIn(cookie,json.dumps(login))
                read=await service.call("browser_read",{"action":"read"},base,trusted)
                self.assertEqual(read["status"],"success",read)
                self.assertIn("Fixture ready",read["text"])
                self.assertNotIn("PRIVATE_VALUE",read["text"])
                clicked=await service.call("browser_input",{"action":"click","selector":"#go"},base,trusted)
                self.assertEqual(clicked["status"],"success",clicked)
                read=await service.call("browser_read",{"action":"read"},base,trusted)
                self.assertIn("Clicked",read["text"])
                shot=await service.call("browser_read",{"action":"screenshot"},base,trusted)
                self.assertEqual(shot["status"],"success",shot)
                image=service.read_artifact(base,trusted,shot["artifact_id"])
                self.assertTrue(image.startswith(b"\x89PNG"))
            finally:await service.close_all(base)
        server.shutdown();server.server_close();thread.join()


class PackageTests(unittest.IsolatedAsyncioTestCase):
    async def test_protocol_rejects_unknown_tools_without_browser(self):
        import runpy
        runner=runpy.run_path(str(Path(__file__).with_name("runner.py")))
        with self.assertRaises(ValueError):
            await runner["exchange"](browser,{"protocol":1,"invocation_id":"fixture","tool":"shell"})
    async def test_startup_and_authentication_errors_are_distinct_and_scrubbed(self):
        with tempfile.TemporaryDirectory() as tmp:
            sid=str(uuid.uuid4());base=Path(tmp)/sid;base.mkdir()
            trusted={"session_id":sid,"user_id":"admin","agent_id":"shared_default","cookie":str(uuid.uuid4()),
                     "generation":"fixture","expires_at":time.time()+300,"allowed_session_ids":[sid]}
            def broken(*args):raise RuntimeError(trusted["cookie"])
            service=browser.BrowserService(Path(tmp)/"private",driver_factory=broken)
            failed=await service.call("browser_read",{"action":"navigate","url":"/app"},base,trusted)
            self.assertEqual(failed["error_kind"],"browser_startup_failed")
            self.assertNotIn(trusted["cookie"],json.dumps(failed))
            wrong=Driver();wrong.identity="other"
            service.factory=lambda *args:wrong
            failed=await service.call("browser_read",{"action":"navigate","url":"/app"},base,trusted)
            self.assertEqual(failed["error_kind"],"browser_authentication_failed")
            self.assertTrue(wrong.closed)
if __name__=="__main__":unittest.main()
