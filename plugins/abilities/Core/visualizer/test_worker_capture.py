import asyncio
import importlib.util
from pathlib import Path
import signal
import tempfile
import unittest
from unittest import mock

spec=importlib.util.spec_from_file_location("core_capture_worker",Path(__file__).resolve().parents[4]/"worker.py")
worker=importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)

class CaptureWorkerTests(unittest.IsolatedAsyncioTestCase):
    async def run_capture(self,fail=False):
        process=mock.Mock(pid=12345,returncode=0)
        process.communicate=mock.AsyncMock(return_value=(b'{"status":"success","output":{}}',b''))
        if fail:process.communicate.side_effect=asyncio.TimeoutError
        with tempfile.TemporaryFile() as lock, mock.patch.object(worker.asyncio,'create_subprocess_exec',mock.AsyncMock(return_value=process)) as spawn, mock.patch.object(worker,'terminate',mock.AsyncMock()) as terminate, mock.patch.object(worker.os,'killpg') as kill:
            request={'name':'core_visual_capture','arguments':{'html':'<h1>Fixture</h1>','width':640,'height':480}}
            if fail:
                with self.assertRaises(asyncio.TimeoutError):await worker.execute_locked(request,lock)
            else:
                self.assertEqual((await worker.execute_locked(request,lock))['status'],'success')
            terminate.assert_awaited_once_with(process)
            kill.assert_called_once_with(process.pid,signal.SIGKILL)
            args,kwargs=spawn.call_args
            self.assertEqual(args[0:2],('/usr/bin/python3','-I'))
            self.assertTrue(args[2].endswith('/Core/visualizer/capture.py'))
            self.assertTrue(kwargs['start_new_session'])
            self.assertEqual(set(kwargs['env']),{'PATH','LANG','HOME','TMPDIR'})

    async def test_finished_leader_does_not_leave_its_browser_group(self):
        await self.run_capture()

    async def test_timeout_terminates_only_the_owned_process_group(self):
        await self.run_capture(True)

if __name__=='__main__':unittest.main()
