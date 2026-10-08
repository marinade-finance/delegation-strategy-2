import base64
import csv
import gzip
import json
import os
import struct
import sys
import urllib.request

RPC_URL = os.environ.get("RPC_URL", "https://api.mainnet-beta.solana.com")
PROGRAM = "HistoryJTGbKQD2mRgLZ3XhqHnN811Qpez8X9kCcGHoa"
ACCOUNT_SIZE = 65856
DISCRIMINATOR = bytes.fromhex("cd1908ddfd830292")
CIRC_BUF_ARR = 320
ENTRY_SIZE = 128
MAX_ITEMS = 512
EMPTY_EPOCH = 0xFFFF
UNSET_VERSION = (0xFF, 0xFF, 0xFFFF)
FIRST_EPOCH = 561
LAST_EPOCH = 1010
# StakeNet covers under 15% of DS2 stake here; a partial fill would read as a real client mix.
SKIPPED_EPOCHS = {598}
REGISTRY_MAX = 14
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58(raw):
    n = int.from_bytes(raw, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = B58[r] + out
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + out


def fetch_accounts():
    body = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "getProgramAccounts",
        "params": [PROGRAM, {"encoding": "base64", "commitment": "finalized",
                             "filters": [{"dataSize": ACCOUNT_SIZE}], "withContext": True}],
    }).encode()
    request = urllib.request.Request(RPC_URL, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=600) as response:
        result = json.loads(response.read())["result"]
    print(f"StakeNet snapshot slot {result['context']['slot']}, {len(result['value'])} accounts", file=sys.stderr)
    return [base64.b64decode(account["account"]["data"][0]) for account in result["value"]]


def read_accounts(path):
    with gzip.open(path, "rb") if path.endswith(".gz") else open(path, "rb") as f:
        return list(iter(lambda: f.read(ACCOUNT_SIZE), b""))


def rows(accounts):
    for account in accounts:
        if account[:8] != DISCRIMINATOR:
            continue
        vote_account = b58(account[12:44])
        for i in range(MAX_ITEMS):
            base = CIRC_BUF_ARR + ENTRY_SIZE * i
            (epoch,) = struct.unpack_from("<H", account, base + 8)
            if epoch == EMPTY_EPOCH or not FIRST_EPOCH <= epoch <= LAST_EPOCH or epoch in SKIPPED_EPOCHS:
                continue
            client_id, major, minor = struct.unpack_from("<BBB", account, base + 17)
            (patch,) = struct.unpack_from("<H", account, base + 20)
            # Above the registry is unset (255) or a u8-truncated id.
            if client_id > REGISTRY_MAX:
                continue
            version = "" if (major, minor, patch) == UNSET_VERSION else f"{major}.{minor}.{patch}"
            yield vote_account, epoch, client_id, version


def main(out_csv, accounts_path=None):
    accounts = read_accounts(accounts_path) if accounts_path else fetch_accounts()
    with gzip.open(out_csv, "wt", newline="") as out:
        writer = csv.writer(out)
        writer.writerow(["vote_account", "epoch", "client_id", "version"])
        latest = {(vote_account, epoch): row for vote_account, epoch, *row in rows(accounts)}
        writer.writerows([*key, *row] for key, row in sorted(latest.items()))


if __name__ == "__main__":
    main(*sys.argv[1:])
