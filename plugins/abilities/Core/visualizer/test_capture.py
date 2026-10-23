import importlib.util
from pathlib import Path
import struct
import sys
import types
import unittest
spec=importlib.util.spec_from_file_location("mini_capture",Path(__file__).with_name("capture.py"))
capture=importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)

class Driver:
    def __init__(self):self.calls=[];self.current_url="";self.clicked=[];self.closed=False
    def execute_cdp_cmd(self,name,args):self.calls.append((name,args))
    def get(self,url):self.current_url=url
    def execute_script(self,source):return [640,480] if 'innerWidth' in source else 'Count: 1'
    def find_element(self,by,selector):self.clicked.append(selector);return types.SimpleNamespace(click=lambda:None)
    def get_screenshot_as_png(self):return b'\x89PNG\r\n\x1a\n'+struct.pack('>I',13)+b'IHDR'+struct.pack('>II',640,480)
    def get_log(self,kind):return []
    def quit(self):self.closed=True

class CaptureTests(unittest.TestCase):
    def test_offline_capture_has_no_origin_bypass_and_checks_actual_viewport(self):
        driver=Driver();calls=[]
        def factory(profile,origin,*,offline):calls.append((origin,offline));return driver
        fake=types.ModuleType('selenium.webdriver.common.by');fake.By=types.SimpleNamespace(CSS_SELECTOR='css selector')
        old=sys.modules.get('selenium.webdriver.common.by');sys.modules['selenium.webdriver.common.by']=fake
        try:result=capture.render({'html':'<button id=counter>Count</button>','width':640,'height':480,'click_selectors':['#counter']},factory)
        finally:
            if old is None:sys.modules.pop('selenium.webdriver.common.by',None)
            else:sys.modules['selenium.webdriver.common.by']=old
        self.assertEqual(calls,[('https://core-visual.invalid',True)])
        self.assertEqual(result['output']['observation'],'Count: 1')
        self.assertEqual(driver.clicked,['#counter']);self.assertTrue(driver.closed)
        self.assertIn(('Network.setBlockedURLs',{'urls':['http://*','https://*','ws://*','wss://*','ftp://*','file://*']}),driver.calls)

    def test_unbounded_source_viewports_and_actions_are_rejected(self):
        for request in [{'html':'x'*524289},{'html':'x','width':1},{'html':'x','click_selectors':['x']*11}]:
            with self.assertRaises(ValueError):capture.render(request)

if __name__=='__main__':unittest.main()
