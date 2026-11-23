"""Fake Clerk for offline testing (no Clerk account needed).

  python3 dev/mock_clerk.py serve              # JWKS + Backend API on :9998
  python3 dev/mock_clerk.py token user_abc     # prints a signed session token for that user
  python3 dev/mock_clerk.py token user_abc --expired | --azp https://evil.example | --kid other
  python3 dev/mock_clerk.py svix <whsec_secret> '<json body>' [--age SECONDS]   # prints svix-* headers

It signs tokens with a throw-away RSA key kept in /tmp (the same key the server publishes as JWKS), mirroring the
shape of real Clerk session tokens (RS256, claims sub/sid/azp/iss/exp/nbf/iat).
"""
import base64, hashlib, hmac, json, os, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer
import jwt
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import rsa

KEY_FILE = "/tmp/mock_clerk_key.pem"
DELETED = "/tmp/mock_clerk_deleted.txt"
ISSUER = os.environ.get("MOCK_CLERK_ISSUER", "https://clerk.mock.test")
SECRET_KEY = "sk_test_mock"
KID = "mock-kid-1"

def load_key():
    if not os.path.exists(KEY_FILE):
        k = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        open(KEY_FILE, "wb").write(k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    return serialization.load_pem_private_key(open(KEY_FILE, "rb").read(), None)

def b64u(n):
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()

def jwks():
    nums = load_key().public_key().public_numbers()
    return {"keys": [{"kty": "RSA", "alg": "RS256", "use": "sig", "kid": KID, "n": b64u(nums.n), "e": b64u(nums.e)}]}

def mint(sub, expired=False, azp="http://localhost:3000", kid=KID):
    now = int(time.time())
    claims = {"sub": sub, "sid": "sess_mock", "iss": ISSUER, "azp": azp, "iat": now - 5, "nbf": now - 5,
              "exp": now - 120 if expired else now + 60, "jti": os.urandom(6).hex(), "v": 2}
    return jwt.encode(claims, load_key(), algorithm="RS256", headers={"kid": kid})

def svix(secret, body, age=0):
    key = base64.b64decode(secret.removeprefix("whsec_"))
    mid, ts = "msg_" + os.urandom(6).hex(), str(int(time.time()) - age)
    sig = base64.b64encode(hmac.new(key, f"{mid}.{ts}.{body}".encode(), hashlib.sha256).digest()).decode()
    return mid, ts, "v1," + sig

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path == "/.well-known/jwks.json": return self._send(200, jwks())
        self._send(404, {})
    def do_DELETE(self):
        if self.headers.get("Authorization") != f"Bearer {SECRET_KEY}": return self._send(401, {"errors": [{"message": "bad key"}]})
        if self.path.startswith("/v1/users/"):
            uid = self.path.rsplit("/", 1)[1]
            open(DELETED, "a").write(uid + "\n")
            return self._send(200, {"object": "deleted_object", "id": uid, "deleted": True})
        self._send(404, {})

if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "serve":
        load_key(); open(DELETED, "w").close()
        HTTPServer(("127.0.0.1", 9998), H).serve_forever()
    elif cmd == "token":
        a = sys.argv[3:]
        print(mint(sys.argv[2], expired="--expired" in a,
                   azp=a[a.index("--azp") + 1] if "--azp" in a else "http://localhost:3000",
                   kid=a[a.index("--kid") + 1] if "--kid" in a else KID))
    elif cmd == "svix":
        a = sys.argv[4:]
        mid, ts, sig = svix(sys.argv[2], sys.argv[3], int(a[a.index("--age") + 1]) if "--age" in a else 0)
        print(mid, ts, sig)
