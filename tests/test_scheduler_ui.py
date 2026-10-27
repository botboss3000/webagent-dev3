"""Opt-in Chromium fixture for actual Builds modules, controls, isolation from receipt filters and touch states."""
import copy,json,os,threading,unittest
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path

class SchedulerUITests(unittest.TestCase):
    """Serve only known candidate UI assets and bounded fake administrator APIs; never authenticate to the app."""
    def test_builds_ownership_and_controls(self):
        """Keep source claims visible, verify closed control requests, pagination and both viewport/theme states."""
        if os.environ.get("V3_BROWSER_FIXTURE")!="1":self.skipTest("Explicit browser fixture required")
        from selenium import webdriver
        from selenium.webdriver.chrome.options import Options
        from selenium.webdriver.common.by import By
        from selenium.webdriver.support.ui import WebDriverWait
        root=Path(__file__).parents[1];requests=[]
        phase_id="f"*64;job_id="11111111-1111-4111-8111-111111111111"
        snapshot={"busy":False,"jobs":[{"job_id":job_id,"summary":"Completed fixture build","status":"completed","phase":"finished","operation":"plugin_deploy","created_at":1,"finished_at":2,"rust_tests":{"status":"not_required"},"phase_history":[]}],
            "containers":{},"leftovers":[],"scheduler":{"mode":"enforce","tasks":[{"id":"a"*64,"label":"holding-code-owner","state":"owned","revision":1,"epoch":1,"generation":2,"scopes":["module:engine"],"requested":[],"owned_by_caller":False}],"total":21,"offset":0,"next_offset":20,"legacy":[]},
            "resources":{"enabled":True,"host":{"available":5*1024**3,"pressure":{"cpu":0,"memory":0}},"reserve":768*1024**2,"phases":{phase_id:{"phase_id":phase_id,"job_id":job_id,"phase":"compile_release","state":"running","revision":0,"priority":0,"estimate":2*1024**3,"paused_seconds":0,"metrics":{"memory":1024**3,"peak_memory":1024**3,"swap":0,"cpu_percent":42}}}}}
        html=b"""<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><style>*{box-sizing:border-box}:root{--border-width:1px;--border:#777;--r-md:6px;--r-lg:10px;--fg-1:#eee;--fg-2:#ccc;--fg-3:#aaa;--bg-elev:#222;--bg-1:#111;--accent:#7ad;--accent-soft:#456}body{margin:0;padding:12px;background:#111;color:#eee;font:14px sans-serif;box-sizing:border-box}.light-mode{background:#fff;color:#222;--fg-1:#222;--fg-2:#444;--fg-3:#666;--bg-elev:#fff;--bg-1:#eee;--accent-soft:#def}button{font:inherit}</style><link rel="stylesheet" href="/ui/main-panel/builds/builds.css"><main id="tab-builds"></main><script type="module">import{mountBuilds}from'/ui/main-panel/app/builds.js';window.pulses=0;Object.defineProperty(navigator,'vibrate',{value:()=>{window.pulses++;return true},configurable:true});window.dispose=mountBuilds(document.querySelector('main'));</script>"""
        files={n:"ui/main-panel/"+n for n in ["app/builds.js","builds/scheduler.js","builds/builds.css"]}
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args):
                """Keep fixture requests and its CSRF placeholder out of test logs."""
            def reply(self,status,body,kind="application/json"):
                """Return one bounded fixture response without exposing candidate source directories."""
                data=body if isinstance(body,bytes)else json.dumps(body).encode()
                self.send_response(status);self.send_header("Content-Type",kind);self.end_headers();self.wfile.write(data)
            def do_GET(self):
                """Expose only named assets, one fixture page and content-free administrator metadata."""
                path=self.path.split("?",1)[0]
                if path=="/fixture":return self.reply(200,html,"text/html")
                if path=="/api/v3/auth/me":return self.reply(200,{"is_admin":True,"csrf_token":"fixture-csrf"})
                if path=="/api/v3/admin/builds":return self.reply(200,snapshot)
                relative=path.removeprefix("/ui/main-panel/")
                if relative in files:return self.reply(200,(root/files[relative]).read_bytes(),"text/css"if relative.endswith(".css")else"text/javascript")
                return self.reply(404,{"error":"fixture route unavailable"})
            def do_POST(self):
                """Assert CSRF and revision fences independently of the client before a fixture effect."""
                if self.path!="/api/v3/admin/builds/scheduler"or self.headers.get("x-csrf-token")!="fixture-csrf":return self.reply(403,{"error":"fixture authority denied"})
                args=json.loads(self.rfile.read(int(self.headers["Content-Length"])));requests.append(args)
                if args["action"]=="reservations":
                    result=copy.deepcopy(snapshot["scheduler"])
                    if args.get("offset")==20:result.update(tasks=[{**result["tasks"][0],"label":"last-reservation-page"}],offset=20,next_offset=None)
                    return self.reply(200,result)
                phase=snapshot["resources"]["phases"][phase_id]
                if args.get("phase_id")!=phase_id or args.get("expected_revision")!=phase["revision"]:return self.reply(409,{"error":"stale phase fixture"})
                self.assert_control_args(args)
                phase["revision"]+=1;phase["state"]="parked"if args["action"]=="pause"else"running"
                return self.reply(200,{"phase_id":phase_id,"job_id":job_id,"state":phase["state"],"revision":phase["revision"]})
            def assert_control_args(self,args):
                """Reject any attempt to send host commands, PIDs or arbitrary ownership metadata."""
                if set(args)!={"action","job_id","phase_id","expected_revision","request_id"}or args["action"]not in {"pause","resume"}:raise AssertionError("Fixture control contract changed")
        server=ThreadingHTTPServer(("127.0.0.1",0),Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        options=Options()
        for flag in ["--headless=new","--no-sandbox","--disable-dev-shm-usage","--window-size=1200,1000"]:options.add_argument(flag)
        driver=webdriver.Chrome(options=options);wait=WebDriverWait(driver,15)
        def text_button(text):
            """Select the actual enabled control by accessible visible text."""
            return driver.find_element(By.XPATH,"//button[normalize-space(.)='"+text+"']")
        try:
            driver.get("http://127.0.0.1:"+str(server.server_port)+"/fixture")
            tasks=wait.until(lambda d:d.find_element(By.CSS_SELECTOR,"details.build-scheduler-tasks"))
            self.assertIsNone(tasks.get_attribute("open"))
            tasks.find_element(By.TAG_NAME,"summary").click()
            phase_section=driver.find_element(By.CSS_SELECTOR,"details.build-scheduler-phases")
            self.assertIsNone(phase_section.get_attribute("open"))
            phase_section.find_element(By.TAG_NAME,"summary").click()
            wait.until(lambda d:"holding-code-owner"in d.find_element(By.CSS_SELECTOR,".build-scheduler-tasks").text)
            text_button("Clear").click()
            wait.until(lambda d:not d.find_elements(By.CSS_SELECTOR,".build-list .build-card"))
            self.assertIn("holding-code-owner",driver.find_element(By.CSS_SELECTOR,".build-scheduler-tasks").text)
            self.assertFalse(driver.find_elements(By.XPATH,"//button[normalize-space(.)='Verify and complete']"))
            pause=text_button("Pause compiler")
            before=driver.execute_script("return getComputedStyle(arguments[0]).borderColor",pause)
            driver.execute_script("arguments[0].dispatchEvent(new PointerEvent('click',{bubbles:true,pointerType:'touch',detail:1}))",pause)
            wait.until(lambda d:bool(d.find_elements(By.XPATH,"//button[normalize-space(.)='Resume when safe']")))
            self.assertEqual(driver.execute_script("return window.pulses"),1)
            self.assertEqual(requests[-1]["action"],"pause")
            self.assertEqual(driver.execute_script("return getComputedStyle(arguments[0]).borderColor",text_button("Resume when safe")),before)
            text_button("Resume when safe").click()
            wait.until(lambda d:bool(d.find_elements(By.XPATH,"//button[normalize-space(.)='Pause compiler']")))
            self.assertEqual(driver.execute_script("return window.pulses"),1)
            text_button("Next source tasks").click()
            wait.until(lambda d:"last-reservation-page"in d.find_element(By.CSS_SELECTOR,".build-scheduler-tasks").text)
            for light,width in [(False,1200),(True,1200),(False,390),(True,390)]:
                driver.set_window_size(width,1000)
                driver.execute_cdp_cmd("Emulation.setDeviceMetricsOverride",{"width":width,"height":1000,"deviceScaleFactor":1,"mobile":width<700})
                driver.execute_cdp_cmd("Emulation.setTouchEmulationEnabled",{"enabled":width<700})
                self.assertEqual(driver.execute_script("return innerWidth"),width)
                if width<700:self.assertTrue(driver.execute_script("return matchMedia('(pointer: coarse)').matches"))
                driver.execute_script("document.body.classList.toggle('light-mode',arguments[0])",light)
                self.assertLessEqual(driver.execute_script("return document.documentElement.scrollWidth"),driver.execute_script("return innerWidth")+1)
                control=text_button("Pause compiler")
                self.assertNotEqual(driver.execute_script("return getComputedStyle(arguments[0]).borderStyle",control),"none")
            evidence=Path(os.environ.get("V3_UI_FIXTURE_OUTPUT","/tmp"))/"scheduler-ui-fixture.png"
            driver.save_screenshot(str(evidence))
            driver.execute_script("window.dispose()")
        finally:
            driver.quit();server.shutdown();server.server_close();thread.join(timeout=2)

if __name__=="__main__":unittest.main()
