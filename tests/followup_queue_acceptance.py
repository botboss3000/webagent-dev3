"""Bounded V3 development acceptance; credentials are never persisted or printed."""
import argparse
import http.cookies
import json
import os
import time
import urllib.error
import urllib.request
import uuid

ORIGIN = "http://100.112.23.15:8081"

class Client:
    def __init__(self):
        self.cookie = None
        self.csrf = None
        data, headers = self.request("POST", "/api/v3/auth/login", {
            "username": "admin", "password": os.environ["V3_TEST_PASSWORD"]}, login=True)
        cookie = http.cookies.SimpleCookie()
        for value in headers.get_all("Set-Cookie", []):
            cookie.load(value)
        self.cookie = "v3_session=" + cookie["v3_session"].value
        self.csrf = data["csrf_token"]

    def request(self, method, path, body=None, login=False):
        headers = {"Content-Type": "application/json"}
        if self.cookie:
            headers["Cookie"] = self.cookie
            headers["X-CSRF-Token"] = self.csrf
        req = urllib.request.Request(ORIGIN + path,
            data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=25) as r:
                data = json.load(r)
                return (data, r.headers) if login else data
        except urllib.error.HTTPError as e:
            raise RuntimeError("API status " + str(e.code)) from None

    def get(self, path):
        return self.request("GET", path)

    def post(self, path, body):
        return self.request("POST", path, body)

    def session(self):
        return self.post("/api/v3/sessions", {"client_request_id": str(uuid.uuid4()),
            "agent_id": "shared_default", "title": "Followup queue acceptance"})["id"]

    def send(self, sid, text):
        return self.post("/api/v3/sessions/" + sid + "/messages", {
            "client_message_id": str(uuid.uuid4()), "text": text})

    def snapshot(self, sid):
        return self.get("/api/v3/sessions/" + sid)

    def wait(self, sid, predicate, timeout=120):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            data = self.snapshot(sid)
            if predicate(data):
                return data
            time.sleep(0.4)
        raise RuntimeError("Bounded acceptance wait expired")

    def choose(self, sid, result, choice):
        submission = result["submission_id"]
        data = self.snapshot(sid)
        entry = next(e for e in data["queue"] if e["id"] == submission)
        return self.request("PATCH", "/api/v3/sessions/" + sid + "/messages/" + submission + "/delivery",
            {"expected_revision": entry["revision"], "client_request_id": str(uuid.uuid4()), "choice": choice})

LONG = "Without tools, explain database transactions in 650 words. This is a disposable chat acceptance test."

def entry(data, result):
    return next(e for e in data["queue"] if e["id"] == result["submission_id"])

def delivered(data, results):
    return all(entry(data, r)["status"] == "delivered" for r in results)

def final(data, rid):
    return any(r["id"] == rid and r["status"] == "completed" for r in data["runs"])

def exercise(client, mode):
    sid = client.session()
    print(json.dumps({"case":mode,"session_id":sid,"started":True}), flush=True)
    first = client.send(sid, LONG)
    rid = first["run_id"]
    client.wait(sid, lambda d: any(r["id"] == rid and r["status"] == "running" for r in d["runs"]))
    a = client.send(sid, "Replace the earlier 650-word answer request. Reply in one sentence containing the exact literal QUEUE_ALPHA.")
    b = client.send(sid, "For the replacement answer, include BOTH exact literals by replying with exactly this one sentence: QUEUE_ALPHA QUEUE_BETA.")
    assert a.get("submission_id") and b.get("submission_id"), "Active sends must queue"
    if mode == "steer":
        client.choose(sid, a, "steer")
        client.choose(sid, b, "steer")
    elif mode == "interrupt":
        client.choose(sid, b, "steer")
        client.choose(sid, a, "interrupt")
    elif mode == "stop":
        client.post("/api/v3/runs/" + rid + "/cancel", {"client_request_id": str(uuid.uuid4())})
        data = client.wait(sid, lambda d: d["queue_state"]["held"])
        assert all(entry(data, r)["status"] != "delivered" for r in [a,b])
        client.post("/api/v3/sessions/" + sid + "/queue/resume", {"client_request_id": str(uuid.uuid4()), "expected_revision": data["queue_state"]["revision"]})
    data = client.wait(sid, lambda d: delivered(d, [a,b]))
    ar = entry(data, a)["consuming_run_id"]
    br = entry(data, b)["consuming_run_id"]
    assert ar == br, "The two messages must share their consuming run"
    if mode == "steer":
        assert ar == rid, "Steering must stay in current run"
    else:
        assert ar != rid, "Queued or interrupt delivery must use subsequent run"
    data = client.wait(sid, lambda d: final(d, ar))
    replies = [m for m in data["messages"] if m["role"] == "assistant" and m["run_id"] == ar]
    assert len(replies) == 1, "Exactly one terminal reply"
    assert "QUEUE_ALPHA" in replies[0]["content"] and "QUEUE_BETA" in replies[0]["content"], "Model received both messages"
    assert len([m for m in data["messages"] if m["role"] == "user"]) == 3, "No duplicated batch bubble"
    print(json.dumps({"case":mode,"session_id":sid,"consuming_run_id":ar,"pass":True}), flush=True)

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", choices=["queue","steer","interrupt","stop"], required=True)
    args = parser.parse_args()
    exercise(Client(), args.case)
