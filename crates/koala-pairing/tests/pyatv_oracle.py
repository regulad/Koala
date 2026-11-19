"""Pairs Koala's pair-setup and pair-verify against pyatv's, both ways.

pyatv (MIT) pairs with real Apple TVs, so agreeing with it is the first test
of Koala's encodings. Its client is `SRPAuthHandler`; its Companion test
server (`protocols/companion/server_auth.py`) shows the server side on
srptools, which this script follows with fresh keys each round. The
signatures pyatv itself skips are checked here with `cryptography`.

    python -I pyatv_oracle.py <path to pair_stdio> [rounds]

Needs pyatv (0.18.0) installed; `pair_stdio` is
`cargo build -p koala-pairing --example pair_stdio`. Many rounds, because
about 1 in 256 SRP values starts with a zero byte, where encodings differ.
"""

import binascii
import os
import subprocess
import sys

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from pyatv.auth.hap_srp import SRPAuthHandler, hkdf_expand
from pyatv.auth.hap_tlv8 import TlvValue, read_tlv, write_tlv
from pyatv.protocols.companion.server_auth import generate_keys, new_server_session
from pyatv.support import opack

RAW = serialization.Encoding.Raw, serialization.PublicFormat.Raw


def seal(key, label, data):
    return ChaCha20Poly1305(key).encrypt(b"\0\0\0\0" + label, data, None)


def unseal(key, label, data):
    return ChaCha20Poly1305(key).decrypt(b"\0\0\0\0" + label, data, None)


class Koala:
    """pair_stdio, one message per line."""

    def __init__(self, exe, role, code, ident):
        self.p = subprocess.Popen(
            [exe, role, code, ident], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
        )
        word, ident_hex, key_hex = self.line().split()
        assert word == "ID"
        self.id, self.ltpk = bytes.fromhex(ident_hex), bytes.fromhex(key_hex)

    def line(self):
        line = self.p.stdout.readline().strip()
        if line.startswith("ERROR"):
            raise RuntimeError(f"Koala: {line}")
        return line

    def recv(self):
        return read_tlv(bytes.fromhex(self.line()))

    def send(self, tlv):
        self.p.stdin.write(write_tlv(tlv).hex() + "\n")
        self.p.stdin.flush()

    def report(self, word):
        parts = self.line().split()
        assert parts[0] == word, parts
        return [bytes.fromhex(p) if p != "-" else None for p in parts[1:]]

    def close(self):
        self.p.stdin.close()
        self.p.wait(10)


def pyatv_client_koala_server(exe, code):
    koala = Koala(exe, "server", code, "koala-server")
    h = SRPAuthHandler()
    h.initialize()
    h.step1(code)
    koala.send({TlvValue.Method: b"\x00", TlvValue.SeqNo: b"\x01"})
    m2 = koala.recv()
    assert m2[TlvValue.SeqNo] == b"\x02"
    a_pub, proof = h.step2(m2[TlvValue.PublicKey], m2[TlvValue.Salt])
    koala.send({TlvValue.SeqNo: b"\x03", TlvValue.PublicKey: a_pub, TlvValue.Proof: proof})
    m4 = koala.recv()
    assert TlvValue.Error not in m4, "Koala refused pyatv's proof"
    assert h._session.verify_proof(binascii.hexlify(m4[TlvValue.Proof])), "Koala's M2 proof is wrong"
    encrypted = h.step3(name="pyatv")
    koala.send({TlvValue.SeqNo: b"\x05", TlvValue.EncryptedData: encrypted})
    m6 = koala.recv()
    creds = h.step4(m6[TlvValue.EncryptedData])

    # pyatv doesn't check M6's signature: do it here.
    k = binascii.unhexlify(h._session.key)
    inner = read_tlv(unseal(hkdf_expand("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info", k), b"PS-Msg06", m6[TlvValue.EncryptedData]))
    x = hkdf_expand("Pair-Setup-Accessory-Sign-Salt", "Pair-Setup-Accessory-Sign-Info", k)
    Ed25519PublicKey.from_public_bytes(inner[TlvValue.PublicKey]).verify(
        inner[TlvValue.Signature], x + inner[TlvValue.Identifier] + inner[TlvValue.PublicKey]
    )
    assert inner[TlvValue.Identifier] == koala.id and inner[TlvValue.PublicKey] == koala.ltpk

    peer_id, peer_key, info = koala.report("PAIRED")
    assert peer_id == h.pairing_id and peer_key == h._auth_public
    assert opack.unpack(info)[0] == {"name": "pyatv"}

    koala.send({TlvValue.SeqNo: b"\x01", TlvValue.PublicKey: h._public_bytes})
    m2 = koala.recv()
    m3 = h.verify1(creds, m2[TlvValue.PublicKey], m2[TlvValue.EncryptedData])
    koala.send({TlvValue.SeqNo: b"\x03", TlvValue.EncryptedData: m3})
    assert koala.recv()[TlvValue.SeqNo] == b"\x04"
    (shared,) = koala.report("VERIFIED")
    assert shared == h._shared
    koala.close()


def pyatv_server_koala_client(exe, code):
    koala = Koala(exe, "client", code, "koala-client")
    keys = generate_keys(os.urandom(32))
    server_id = b"pyatv-server"
    session, salt = new_server_session(keys, code)

    m1 = koala.recv()
    assert m1[TlvValue.SeqNo] == b"\x01" and m1[TlvValue.Method] == b"\x00"
    koala.send({TlvValue.SeqNo: b"\x02", TlvValue.Salt: binascii.unhexlify(salt), TlvValue.PublicKey: binascii.unhexlify(session.public)})
    m3 = koala.recv()
    session.process(binascii.hexlify(m3[TlvValue.PublicKey]).decode(), salt)
    assert session.verify_proof(binascii.hexlify(m3[TlvValue.Proof])), "Koala's M1 proof is wrong"
    koala.send({TlvValue.SeqNo: b"\x04", TlvValue.Proof: binascii.unhexlify(session.key_proof_hash)})

    m5 = koala.recv()
    k = binascii.unhexlify(session.key)
    enc_key = hkdf_expand("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info", k)
    inner = read_tlv(unseal(enc_key, b"PS-Msg05", m5[TlvValue.EncryptedData]))
    x = hkdf_expand("Pair-Setup-Controller-Sign-Salt", "Pair-Setup-Controller-Sign-Info", k)
    client_ltpk = inner[TlvValue.PublicKey]
    Ed25519PublicKey.from_public_bytes(client_ltpk).verify(inner[TlvValue.Signature], x + inner[TlvValue.Identifier] + client_ltpk)
    assert inner[TlvValue.Identifier] == koala.id and client_ltpk == koala.ltpk

    x = hkdf_expand("Pair-Setup-Accessory-Sign-Salt", "Pair-Setup-Accessory-Sign-Info", k)
    info = opack.pack({"name": "pyatv", "model": "AppleTV6,2"})
    m6 = write_tlv(
        {
            TlvValue.Identifier: server_id,
            TlvValue.PublicKey: keys.auth_pub,
            TlvValue.Signature: keys.sign.sign(x + server_id + keys.auth_pub),
            TlvValue.Name: info,
        }
    )
    koala.send({TlvValue.SeqNo: b"\x06", TlvValue.EncryptedData: seal(enc_key, b"PS-Msg06", m6)})
    peer_id, peer_key, peer_info = koala.report("PAIRED")
    assert peer_id == server_id and peer_key == keys.auth_pub and peer_info == info

    m1 = koala.recv()
    client_eph = m1[TlvValue.PublicKey]
    eph = X25519PrivateKey.generate()
    eph_pub = eph.public_key().public_bytes(*RAW)
    shared = eph.exchange(X25519PublicKey.from_public_bytes(client_eph))
    key = hkdf_expand("Pair-Verify-Encrypt-Salt", "Pair-Verify-Encrypt-Info", shared)
    sealed = seal(key, b"PV-Msg02", write_tlv({TlvValue.Identifier: server_id, TlvValue.Signature: keys.sign.sign(eph_pub + server_id + client_eph)}))
    koala.send({TlvValue.SeqNo: b"\x02", TlvValue.PublicKey: eph_pub, TlvValue.EncryptedData: sealed})
    m3 = koala.recv()
    inner = read_tlv(unseal(key, b"PV-Msg03", m3[TlvValue.EncryptedData]))
    assert inner[TlvValue.Identifier] == koala.id
    Ed25519PublicKey.from_public_bytes(client_ltpk).verify(inner[TlvValue.Signature], client_eph + koala.id + eph_pub)
    koala.send({TlvValue.SeqNo: b"\x04"})
    (koala_shared,) = koala.report("VERIFIED")
    assert koala_shared == shared
    koala.close()


def main():
    exe = sys.argv[1]
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 50
    failures = 0
    for name, run in [("pyatv client -> Koala server", pyatv_client_koala_server), ("Koala client -> pyatv server", pyatv_server_koala_client)]:
        ok = 0
        for i in range(rounds):
            code = f"{i % 10000:04d}"
            try:
                run(exe, code)
                ok += 1
            except Exception as e:  # noqa: BLE001 - report every kind of failure
                failures += 1
                print(f"{name}, round {i}: {type(e).__name__}: {e}")
        print(f"{name}: {ok}/{rounds}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
