"""pyatv's own Companion client against Koala's Rapport server.

pyatv (MIT) speaks Rapport to real Apple TVs. Here its client stack, with no
changes, pairs with `examples/rapport_server`, verifies on a new connection,
and sends a `_systemInfo` request over the encrypted session: framing,
pairing, encryption and messages, end to end.

    python -I pyatv_client.py <path to rapport_server> [rounds]

Needs pyatv 0.18.0 with srptools below 2.0.
"""

import asyncio
import subprocess
import sys

from pyatv.auth.hap_srp import SRPAuthHandler
from pyatv.const import Protocol
from pyatv.core import MutableService
from pyatv.protocols.companion.auth import CompanionPairSetupProcedure
from pyatv.protocols.companion.connection import CompanionConnection, FrameType
from pyatv.protocols.companion.protocol import CompanionProtocol


async def one_round(port, code):
    loop = asyncio.get_running_loop()
    service = MutableService(None, Protocol.Companion, port, {})

    # Pair-setup, as `atvremote pair` does.
    srp = SRPAuthHandler()
    protocol = CompanionProtocol(CompanionConnection(loop, "127.0.0.1", port), srp, service)
    setup = CompanionPairSetupProcedure(protocol, srp)
    await setup.start_pairing()
    credentials = await setup.finish_pairing("", code, "pyatv")
    protocol.stop()

    # A new connection: pair-verify with those credentials, then a request.
    service.credentials = str(credentials)
    srp = SRPAuthHandler()
    protocol = CompanionProtocol(CompanionConnection(loop, "127.0.0.1", port), srp, service)
    await protocol.start()
    reply = await protocol.exchange_opack(FrameType.E_OPACK, {"_i": "_systemInfo", "_t": 2, "_c": {"name": "pyatv"}})
    protocol.stop()
    assert reply["_t"] == 3, reply
    assert reply["_c"]["_i"] == "_systemInfo", reply


def main():
    exe = sys.argv[1]
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 20
    code = "1234"
    server = subprocess.Popen([exe, "0", code], stdout=subprocess.PIPE, text=True)
    try:
        word, port = server.stdout.readline().split()
        assert word == "LISTENING"
        failures = 0
        for i in range(rounds):
            try:
                asyncio.run(one_round(int(port), code))
            except Exception as e:  # noqa: BLE001 - report every kind of failure
                failures += 1
                print(f"round {i}: {type(e).__name__}: {e}")
        print(f"pyatv's Companion client -> Koala's Rapport server: {rounds - failures}/{rounds}")
    finally:
        server.kill()
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
