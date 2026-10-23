"""Offline mini-app capture. This child process receives no cookies, account keys or workspace."""
import base64
import contextlib
import importlib.util
import json
import sys
import tempfile
from pathlib import Path


def render(request, factory=None):
    html=request.get("html")
    width=request.get("width",1280)
    height=request.get("height",800)
    clicks=request.get("click_selectors",[])
    if (type(html) is not str or not 1<=len(html.encode())<=524288
            or type(width) is not int or not 320<=width<=1600
            or type(height) is not int or not 240<=height<=1200
            or type(clicks) is not list or len(clicks)>10
            or any(type(s) is not str or not s or len(s)>200 for s in clicks)):
        raise ValueError("Invalid bounded mini app capture")
    path=Path(__file__).resolve().parents[2]/"Administrator/diagnostics/adapter.py"
    spec=importlib.util.spec_from_file_location("core_visual_browser",path)
    backend=importlib.util.module_from_spec(spec)
    spec.loader.exec_module(backend)
    with tempfile.TemporaryDirectory(prefix="webagent-core-visual-") as folder:
        driver=None
        try:
            driver=(factory or backend.chrome_factory)(Path(folder),"https://core-visual.invalid",offline=True)
            driver.execute_cdp_cmd("Network.enable",{})
            driver.execute_cdp_cmd("Network.setBlockedURLs",{"urls":["http://*","https://*","ws://*","wss://*","ftp://*","file://*"]})
            driver.execute_cdp_cmd("Emulation.setDeviceMetricsOverride",{"width":width,"height":height,"deviceScaleFactor":1,"mobile":False})
            driver.get("data:text/html;base64,"+base64.b64encode(html.encode()).decode())
            viewport=driver.execute_script("return [innerWidth,innerHeight]")
            if viewport!=[width,height]:
                raise ValueError("Browser viewport did not match requested bounds")
            from selenium.webdriver.common.by import By
            for selector in clicks:
                if not driver.current_url.startswith("data:text/html"):
                    raise ValueError("Mini app changed document")
                driver.find_element(By.CSS_SELECTOR,selector).click()
            if not driver.current_url.startswith("data:text/html"):
                raise ValueError("Mini app changed document")
            text=driver.execute_script("return (document.body?.innerText||'').slice(0,4096)")
            png=driver.get_screenshot_as_png()
            if len(png)>1024*1024:raise ValueError("Capture PNG exceeded bounds")
            metadata=backend.png_metadata(png)
            if metadata["width"]!=width or metadata["height"]!=height:
                raise ValueError("Screenshot viewport did not match requested bounds")
            text=str(text).encode("utf-8","replace")[:4096].decode("utf-8","ignore")
            logs=[]
            with contextlib.suppress(Exception):
                logs=[{"level":str(v.get("level",""))[:16],"message":str(v.get("message","")).encode("utf-8","replace")[:300].decode("utf-8","ignore")} for v in driver.get_log("browser")[:20]]
            return {"status":"success","output":{**metadata,"png_base64":base64.b64encode(png).decode(),"observation":text,"console":logs}}
        finally:
            if driver is not None:
                with contextlib.suppress(Exception):driver.quit()


if __name__=="__main__":
    try:
        raw=sys.stdin.buffer.read(786433)
        if len(raw)>786432:raise ValueError("Capture input too large")
        result=render(json.loads(raw))
    except Exception:
        # Driver exceptions can include complete data URLs. Never echo them.
        result={"status":"failed","output":"Isolated mini app capture failed"}
    sys.stdout.write(json.dumps(result))
