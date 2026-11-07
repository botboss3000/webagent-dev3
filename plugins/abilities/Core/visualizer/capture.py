"""Offline mini-app capture. This child process receives no cookies, account keys or workspace."""
import base64
import contextlib
import hashlib
import json
import logging
import struct
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

MAX_PNG=1024*1024


def png_metadata(data):
    """Verify PNG bytes and dimensions before returning a capture."""
    if not isinstance(data,bytes) or not 24<=len(data)<=MAX_PNG or data[:8]!=b"\x89PNG\r\n\x1a\n" or data[12:16]!=b"IHDR":
        raise ValueError("Screenshot is not a bounded PNG")
    width,height=struct.unpack(">II",data[16:24])
    if not 1<=width<=1600 or not 1<=height<=1200:raise ValueError("Screenshot dimensions exceed viewport bounds")
    return {"media_type":"image/png","bytes":len(data),"width":width,"height":height,
        "sha256":hashlib.sha256(data).hexdigest()}


def chrome_factory(profile,origin,*,offline=False):
    """Start Chromium with private storage and no network beyond loopback when offline.

    Kept from the former Browser Control Python adapter (now the worker's Rust browser
    module); Selenium's debug transport logging includes command bodies, so it is silenced.
    """
    for name in ("selenium","selenium.webdriver.remote.remote_connection","urllib3","urllib3.connectionpool"):
        logger=logging.getLogger(name);logger.setLevel(logging.CRITICAL);logger.propagate=False
    from selenium import webdriver
    from selenium.webdriver.chrome.service import Service
    parsed=urlsplit(origin or "");port=parsed.port or (443 if parsed.scheme=="https" else 80)
    options=webdriver.ChromeOptions()
    options.binary_location="/usr/bin/chromium"
    for argument in ("--headless=new","--no-sandbox","--disable-dev-shm-usage",
        "--disable-background-networking","--disable-extensions","--disable-sync",
        "--no-first-run","--no-default-browser-check","--disk-cache-size=1048576",
        "--media-cache-size=1048576","--window-size=1280,900",
        "--js-flags=--max-old-space-size=512"):
        options.add_argument(argument)
    options.add_argument("--user-data-dir="+str(profile))
    options.add_argument("--proxy-server=http://127.0.0.1:9")
    options.add_argument("--proxy-bypass-list="+("<-loopback>" if offline else "<-loopback>;"+parsed.hostname+":"+str(port)))
    if offline:
        options.add_argument("--host-resolver-rules=MAP * ~NOTFOUND")
        options.add_argument("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
        options.add_argument("--disable-quic")
    options.set_capability("goog:loggingPrefs",{"browser":"ALL"})
    options.add_experimental_option("prefs",{"download_restrictions":3,
        "credentials_enable_service":False,"profile.password_manager_enabled":False})
    service=Service(executable_path="/usr/bin/chromedriver",log_path="/dev/null",
        popen_kw={"start_new_session":False},
        env={"PATH":"/usr/bin:/bin","HOME":str(profile),"TMPDIR":"/tmp",
             "XDG_CONFIG_HOME":str(profile/"config"),"XDG_CACHE_HOME":str(profile/"cache"),"LANG":"C.UTF-8"})
    driver=None
    try:
        driver=webdriver.Chrome(service=service,options=options)
        driver.set_page_load_timeout(10)
        driver.set_script_timeout(5)
        driver.set_window_size(1280,900)
        return driver
    except Exception:
        if driver is not None:
            with contextlib.suppress(Exception):driver.quit()
        with contextlib.suppress(Exception):service.stop()
        raise RuntimeError("Private browser startup failed") from None


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
    with tempfile.TemporaryDirectory(prefix="webagent-core-visual-") as folder:
        driver=None
        try:
            driver=(factory or chrome_factory)(Path(folder),"https://core-visual.invalid",offline=True)
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
            metadata=png_metadata(png)
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
