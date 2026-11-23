"""Tiny fake OpenAI server so the whole stack can be tested offline (no API key, no cost).
Run:  python3 dev/mock_openai.py   then start the backend with
      OPENAI_API_KEY=test OPENAI_BASE_URL=http://127.0.0.1:9999/v1 cargo run
"""
import hashlib, json, math
from http.server import BaseHTTPRequestHandler, HTTPServer

def embed(text, dim=64):
    v = [0.0] * dim
    for w in text.lower().split():
        h = int(hashlib.md5(w.encode()).hexdigest(), 16)
        v[h % dim] += 1.0
    n = math.sqrt(sum(x * x for x in v)) or 1.0
    return [x / n for x in v]

def call(name, args, i=1):
    return {"role": "assistant", "content": None, "tool_calls": [
        {"id": f"call_{i}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}

def chat(messages):
    last = messages[-1]
    if last["role"] == "tool":
        return {"role": "assistant", "content": f"[mock final] tool said: {last['content'][:160]}"}
    text = (last.get("content") or "").lower()
    if "bp" in text and "remind" not in text:
        return call("log_vital", {"kind": "blood_pressure", "value": "130/85", "unit": "mmHg"})
    if "remind" in text:
        return call("create_reminder", {"title": "Take BP tablet", "when": "8 pm daily"})
    if "headache" in text:
        return call("search_health_knowledge", {"query": "headache"})
    if "triage" in text:
        return call("assess_symptoms", {"symptoms": "cough", "severity": 6, "duration_days": 3, "age": 30})
    if "my report" in text:
        return call("search_user_documents", {"query": "hemoglobin"})
    return {"role": "assistant", "content": "[mock] hello"}

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, code, body, ctype="application/json"):
        b = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(code); self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0)); raw = self.rfile.read(n)
        if self.path.endswith("/chat/completions"):
            self._send(200, {"choices": [{"message": chat(json.loads(raw)["messages"])}], "usage": {"total_tokens": 600}})
        elif self.path.endswith("/embeddings"):
            inp = json.loads(raw)["input"]
            self._send(200, {"data": [{"embedding": embed(t)} for t in inp], "usage": {"total_tokens": 10}})
        elif self.path.endswith("/audio/transcriptions"):
            self._send(200, {"text": "mock transcript " + str(len(raw))})
        elif self.path.endswith("/audio/speech"):
            self._send(200, b"ID3mockmp3", "audio/mpeg")
        else:
            self._send(404, {"error": "unknown"})

HTTPServer(("127.0.0.1", 9999), H).serve_forever()
