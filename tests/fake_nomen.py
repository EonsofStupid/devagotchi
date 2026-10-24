"""A fake Nomen for tests: token introspection that checks the caller's private-key JWT,
and the token endpoint of the browser sign-in (authorization code with PKCE).

Standard library only, so it runs on every CI runner. It mints an RSA key for the hub's
API application (written as the JSON key file Nomen downloads) and verifies the hub's
RS256 client assertion with that key, the way Nomen does, before answering about a token.
"""
import base64
import hashlib
import hmac
import json
import math
import secrets
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

JWT_BEARER = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
_SMALL_PRIMES = [p for p in range(3, 2000, 2) if all(p % d for d in range(3, int(p ** 0.5) + 1, 2))]


def _probable_prime(bits):
    while True:
        n = secrets.randbits(bits) | (3 << (bits - 2)) | 1  # top two bits set: p*q has 2*bits bits
        if any(n % p == 0 for p in _SMALL_PRIMES):
            continue
        d, r = n - 1, 0
        while d % 2 == 0:
            d, r = d // 2, r + 1
        for _ in range(40):
            x = pow(secrets.randbelow(n - 3) + 2, d, n)
            if x in (1, n - 1):
                continue
            for _ in range(r - 1):
                x = pow(x, 2, n)
                if x == n - 1:
                    break
            else:
                break
        else:
            return n


def _der_len(n):
    if n < 0x80:
        return bytes([n])
    body = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([0x80 | len(body)]) + body


def _der_int(v):
    body = v.to_bytes(v.bit_length() // 8 + 1, "big")
    return b"\x02" + _der_len(len(body)) + body


def rsa_key(bits=2048):
    """(PKCS#1 PEM, n, e) for a fresh key."""
    e = 65537
    while True:
        p, q = _probable_prime(bits // 2), _probable_prime(bits // 2)
        phi = (p - 1) * (q - 1)
        if p != q and math.gcd(e, phi) == 1:
            break
    n, d = p * q, pow(e, -1, phi)
    fields = (0, n, e, d, p, q, d % (p - 1), d % (q - 1), pow(q, -1, p))
    body = b"".join(_der_int(f) for f in fields)
    der = b"\x30" + _der_len(len(body)) + body
    b64 = base64.b64encode(der).decode()
    lines = "\n".join(b64[i:i + 64] for i in range(0, len(b64), 64))
    return f"-----BEGIN RSA PRIVATE KEY-----\n{lines}\n-----END RSA PRIVATE KEY-----\n", n, e


def _b64url(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def _rs256_valid(signing_input, signature, n, e):
    k = (n.bit_length() + 7) // 8
    if len(signature) != k:
        return False
    em = pow(int.from_bytes(signature, "big"), e, n).to_bytes(k, "big")
    digest_info = bytes.fromhex("3031300d060960864801650304020105000420") + hashlib.sha256(signing_input).digest()
    expected = b"\x00\x01" + b"\xff" * (k - 3 - len(digest_info)) + b"\x00" + digest_info
    return hmac.compare_digest(em, expected)


class FakeNomen:
    """Serves POST /oauth/v2/introspect on 127.0.0.1. `answers` maps token -> answer."""

    def __init__(self, client_id="devagotchi-hub@shippin", key_id="hub-key"):
        self.client_id, self.key_id = client_id, key_id
        self.pem, self.n, self.e = rsa_key()
        self.answers = {}
        # Authorization codes for the browser sign-in: code -> what it was issued for.
        self.codes = {}
        self.token_calls = 0
        self.calls = 0
        self.status = 200
        self.lock = threading.Lock()
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def reply(self, status, body):
                raw = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_POST(self):
                length = int(self.headers.get("Content-Length") or 0)
                form = urllib.parse.parse_qs(self.rfile.read(length).decode())
                if self.path == "/oauth/v2/token":
                    return fake.token(self, form)
                with fake.lock:
                    fake.calls += 1
                    status = fake.status
                if self.path != "/oauth/v2/introspect":
                    return self.reply(404, {"error": "not_found"})
                if status != 200:
                    return self.reply(status, {"error": "unavailable"})
                if not fake.client_ok(form):
                    return self.reply(401, {"error": "invalid_client"})
                token = (form.get("token") or [""])[0]
                with fake.lock:
                    answer = fake.answers.get(token, {"active": False})
                self.reply(200, answer)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.issuer = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def client_ok(self, form):
        if (form.get("client_assertion_type") or [""])[0] != JWT_BEARER:
            return False
        try:
            head, claims, sig = (form.get("client_assertion") or [""])[0].split(".")
            header = json.loads(_b64url(head))
            body = json.loads(_b64url(claims))
            signed = _rs256_valid(f"{head}.{claims}".encode(), _b64url(sig), self.n, self.e)
        except (ValueError, json.JSONDecodeError):
            return False
        now = time.time()
        return (signed and header.get("alg") == "RS256" and header.get("kid") == self.key_id
                and body.get("iss") == self.client_id and body.get("sub") == self.client_id
                and body.get("aud") == self.issuer and body.get("exp", 0) > now
                and body.get("iat", now + 120) <= now + 60)

    def issue_code(self, challenge, access_token, client_id, redirect_uri, expires_in=3600):
        """Play Nomen's authorization step: a single-use code bound to a PKCE challenge."""
        code = secrets.token_urlsafe(18)
        with self.lock:
            self.codes[code] = (challenge, access_token, client_id, redirect_uri, expires_in)
        return code

    def token(self, handler, form):
        """The token endpoint for a public client: the code, once, with its PKCE verifier."""
        field = lambda key: (form.get(key) or [""])[0]  # noqa: E731
        with self.lock:
            self.token_calls += 1
            status = self.status
            issued = self.codes.pop(field("code"), None)
        if status != 200:
            return handler.reply(status, {"error": "unavailable"})
        if issued is None or field("grant_type") != "authorization_code":
            return handler.reply(400, {"error": "invalid_grant"})
        challenge, access_token, client_id, redirect_uri, expires_in = issued
        verifier = field("code_verifier").encode()
        computed = base64.urlsafe_b64encode(hashlib.sha256(verifier).digest()).decode().rstrip("=")
        if field("client_id") != client_id or field("redirect_uri") != redirect_uri or computed != challenge:
            return handler.reply(400, {"error": "invalid_grant"})
        handler.reply(200, {"access_token": access_token, "token_type": "Bearer", "expires_in": expires_in,
                            "id_token": "unused-by-the-hub"})

    def key_file(self):
        """The JSON Nomen downloads for an API application's private-key JWT."""
        return json.dumps({"type": "application", "keyId": self.key_id, "key": self.pem,
                           "appId": "300000000000000900", "clientId": self.client_id})

    def active(self, sub, aud, username=None, **overrides):
        answer = {"active": True, "iss": self.issuer, "aud": aud, "sub": sub, "exp": int(time.time()) + 3600,
                  "client_id": "shippin-mcp", "token_type": "Bearer"}
        if username:
            answer["username"] = username
        answer.update(overrides)
        return answer

    def close(self):
        self.server.shutdown()
