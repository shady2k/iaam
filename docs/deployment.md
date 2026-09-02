# Deployment

The reader of this document is an agent, and the document is written to be
executed. Every step is a command followed by the output that proves it worked.
A step whose success cannot be observed by running something is not a step and
is not in here. Where a step can fail, the failure names what is missing, who
supplies it, and the command that supplies it.

Nothing here is a "sensible location". `/var/lib/iaam/iaam.db`,
`/etc/iaam/broker-key`, the image tag `iaam:0.1.0` and the container name `iaam`
are literal values that the commands below actually pass, and you may replace
them with other literal values — but nothing in the program, the image or this
repository will guess them for you. There are no defaults for a path, and there
will not be: a database in an unexpected place looks exactly like a lost
portfolio.

---

## 1. What is deployed

`crates/iaam-bootstrap` builds one binary, `iaam`. It is the entire deployable
and it has two roles.

| Role | Command | Run by |
|---|---|---|
| HTTP service | `iaam serve` | a service manager, unattended |
| local administration | `iaam claim`, `iaam token issue`, `iaam broker key …`, `iaam broker access …` | the owner, at a console |

The second role is not a convenience wrapper. Under
[ADR-0003](decisions/0003-the-owner-speaks-to-an-agent-and-a-cli-keeps-the-secrets.md)
the CLI owns the trust root and every secret: **no HTTP route issues an owner
token, and no HTTP route accepts a broker credential.** The CLI's authority is
the operating system's — the identity it runs as, the permissions on the
database and key files, and the boundary that decides who may execute it at all.
A deployment where anyone can run `iaam claim` against the database file has
given away ownership of the instance; §3.4 and §4.4 are where that boundary is
actually set.

Three rules follow, and they hold for every step below.

- **A secret never travels through a conversation with an agent.** The owner
  token is printed once on a console. A broker token is pasted into a console on
  standard input. An agent receives its own bearer token from its host's
  configuration and never any other credential.
- **A run-time input is never baked into an image or a committed file.** The
  database path, the bind address, the key file and its contents, and the
  account and counterparty maps the import skills take are supplied when the
  program runs, from outside this repository.
- **The console is where ownership is established.** `iaam claim` prints the
  first owner token exactly once. There is no one-time claim code and no
  `POST /v1/claim`; both were retired with ADR-0003.

---

## 2. Configuration

### 2.1 Every variable the program reads

| Variable | Kind | Default | Read by |
|---|---|---|---|
| `IAAM_DATABASE` | **required** | none — every subcommand refuses without it | every subcommand, including `serve` |
| `IAAM_BROKER_KEY_FILE` | path to a secret | none | `broker key generate`, `broker access add`, `broker access rotate`; optional for `serve` |
| `IAAM_LISTEN` | optional | `127.0.0.1:8080` | `serve` |
| `IAAM_RATE_LIMIT` | optional | `120` | `serve` |
| `IAAM_RATE_WINDOW_SECONDS` | optional | `60` | `serve` |
| `RUST_LOG` | optional | `info` | `serve` |

`IAAM_BROKER_KEY_FILE` is optional for `serve` only in the sense that a service
that never talks to a broker can run without it. If it is set and the file is
absent, `serve` refuses to start rather than starting silently without
encryption. Broker routes on a server started without it answer
`{"code":"not_configured", …}`; the fix is a restart with the key, not a
different call.

### 2.2 Secrets

Never in the image, never in a file committed to this repository, never in a
conversation.

| Secret | Where it lives | How it is created |
|---|---|---|
| broker encryption key | a file outside the database, mode `0600`, e.g. `/etc/iaam/broker-key` | `iaam broker key generate` (§6.1) |
| owner token | the operator's password manager; only its hash is in the database | `iaam claim` (§3.5, §4.4) |
| agent / read-only tokens | the agent host's configuration | `POST /v1/tokens` (§7) |
| the broker's own token | nowhere in configuration — it is pasted on standard input and stored only as ciphertext | `iaam broker access add` (§6.3) |

### 2.3 Run-time inputs that must never be baked in

The database file, the key file, the published bind address, and the account and
counterparty maps used by the import skills (which is why those skills take
`--account-map` and know nothing on their own). They are arguments and mounts,
not image contents.

### 2.4 Variables that are now refused

ADR-0003 replaced the provisioning environment variables with subcommands. The
program refuses to start if one of them is set, and names its replacement.

| Refused variable | Replacement |
|---|---|
| `IAAM_ISSUE_OWNER_TOKEN` | `iaam token issue` |
| `IAAM_ADD_BROKER_ACCESS` | `iaam broker access add` |
| `IAAM_GENERATE_BROKER_KEY` | `iaam broker key generate` |
| `IAAM_BROKER_KEY_OLD_FILE` | `iaam broker key rotate --old <path> --new <path>` |
| `IAAM_BROKER_KEY_NEW_FILE` | `iaam broker key rotate --old <path> --new <path>` |

Check, on either route:

```console
$ IAAM_DATABASE=/var/lib/iaam/iaam.db IAAM_ISSUE_OWNER_TOKEN=console iaam token issue --label console
error: environment variable IAAM_ISSUE_OWNER_TOKEN was replaced by `iaam token issue`
$ echo $?
1
```

If you see this, an old unit file, shell profile or compose file is still
setting it. Remove the variable; the subcommand is the whole replacement.

---

## 3. Route A — container

The image is built from this repository's `Dockerfile`. It contains the binary
and nothing else: no database, no key, no map, no token, no bind address.

### 3.1 Preconditions

```console
$ docker version --format '{{.Server.Version}}'
29.7.2
```

Any version that supports multi-stage builds will do; the number above is what
this was verified against.

**On failure** — `Cannot connect to the Docker daemon` means docker is not
running or your user is not in the `docker` group. Supplied by the machine's
administrator: `sudo systemctl start docker`, then
`sudo usermod --append --groups docker "$USER"` and a new login session.

### 3.2 Build the image

From a clean checkout, with the repository root as the working directory:

```console
$ docker build --tag iaam:0.1.0 .
…
 => => naming to docker.io/library/iaam:0.1.0
$ echo $?
0
```

The build needs network access to crates.io and to the Debian archive. It takes
several minutes the first time and compiles the workspace with `--locked`, so a
`Cargo.lock` that does not match the manifests fails the build instead of
quietly resolving something else.

**On failure** — `failed to solve: … no such file or directory` for
`Cargo.lock` means the checkout is incomplete; the fix is a full `git clone`.
A network error during `cargo build` is the build host's proxy or DNS, supplied
by the machine's administrator.

### 3.3 Prove the image is what it claims

```console
$ docker image inspect iaam:0.1.0 --format 'user={{.Config.User}} entrypoint={{.Config.Entrypoint}} env={{.Config.Env}}'
user=10001:10001 entrypoint=[/usr/local/bin/iaam] env=[PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin]
```

Two things are being checked, and both are requirements rather than trivia. The
user is not root. `env` contains `PATH` and nothing else — no `IAAM_*` variable
is compiled into the image, which is what "configuration, not a default" means
in practice.

```console
$ docker run --rm iaam:0.1.0 --help
The iaam service and local administration CLI

Usage: iaam <COMMAND>

Commands:
  serve   Run the iaam server
  claim   Claim a fresh instance and print its owner token once
  token   Manage API tokens
  broker  Manage broker credentials and access
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

The entrypoint is the binary, so everything after the image name is arguments to
`iaam`.

### 3.4 Create the host directories

The container runs as uid/gid `10001`, and a bind mount keeps the host's
ownership. The directory must therefore belong to that uid, and the number is
fixed in the `Dockerfile` precisely so this command can name it.

```console
$ sudo install --directory --owner 10001 --group 10001 --mode 0700 /var/lib/iaam
$ stat --format '%u %g %a' /var/lib/iaam
10001 10001 700
```

Mode `0700` is the access control. The database file itself is created `0644`;
it is the directory that keeps other users out of it, and this is the step that
decides who can run `iaam claim` against the instance (§1).

**On failure** — output other than `10001 10001 700` means the directory existed
with other ownership. Supplied by the machine's administrator:
`sudo chown 10001:10001 /var/lib/iaam && sudo chmod 0700 /var/lib/iaam`. If it
is skipped, the next step fails with `unable to open database file`.

### 3.5 Claim the instance

This creates the owner and prints the owner token. It happens **once** in the
life of a database.

```console
$ docker run --rm \
    --mount type=bind,source=/var/lib/iaam,target=/var/lib/iaam \
    --env IAAM_DATABASE=/var/lib/iaam/iaam.db \
    iaam:0.1.0 claim --label console
1f0c…  (64 hexadecimal characters, on one line)
```

Record it in the operator's password manager now. Then check that the claim took
effect, by making it a second time:

```console
$ docker run --rm --mount type=bind,source=/var/lib/iaam,target=/var/lib/iaam \
    --env IAAM_DATABASE=/var/lib/iaam/iaam.db iaam:0.1.0 claim --label console
error: instance is already claimed
$ echo $?
1
```

That refusal is the proof the first call took effect. The database now holds one
owner and the hash of one token; the token itself exists only where the operator
put it. There is no command that shows it again. Losing it costs a console
visit (§7.3), not the instance.

**On failure** — `error: SQLite error: unable to open database file:
/var/lib/iaam/iaam.db` is §3.4 not done: the directory exists but the container's
uid cannot write to it. `error: variable IAAM_DATABASE is not set …` is a missing
`--env`, supplied by whoever writes the run command.

### 3.6 Start the service

```console
$ docker run --detach --name iaam --restart unless-stopped \
    --mount type=bind,source=/var/lib/iaam,target=/var/lib/iaam \
    --env IAAM_DATABASE=/var/lib/iaam/iaam.db \
    --env IAAM_LISTEN=0.0.0.0:8080 \
    --publish 127.0.0.1:8080:8080 \
    --read-only --tmpfs /tmp \
    --cap-drop ALL --security-opt no-new-privileges \
    iaam:0.1.0 serve
456d86886aac…
$ docker logs iaam
2026-09-02T15:48:19.889341Z  INFO iaam: server started address=0.0.0.0:8080
```

`IAAM_LISTEN` must be set here, and setting it is not a weakening of the
program's loopback default. Inside a network namespace `127.0.0.1` is reachable
only from that same container, so `--publish` would forward to nothing.
`--publish 127.0.0.1:8080:8080` puts the socket back on the host's loopback,
which is where the default meant it to be. Publishing on `0.0.0.0` instead
exposes an HTTP service that carries bearer tokens in clear text; put a reverse
proxy in front of it first (§8).

`--read-only`, `--cap-drop ALL` and `--security-opt no-new-privileges` are not
decoration: the service writes only to the mounted data directory, and it was
verified to start and serve with all three.

**On failure** — the container exits immediately and `docker logs iaam` holds
the reason. Every message the program can print at start-up is in §11.

### 3.7 Administration afterwards

Every administrative command is the same image with a different argument list
and no `--detach`. The service does not need to be stopped for any of them,
except a change to the key the running server reads, which needs a restart
(§6.1).

---

## 4. Route B — binary on the host

Use this where there is no container runtime, or where the broker key must be
delivered by systemd credentials (§6.6), which is the stronger option.

### 4.1 Build

All commands run inside the project's development environment.

```console
$ nix develop -c cargo build --release --locked --package iaam-bootstrap
    Finished `release` profile [optimized] target(s) in …
$ ls -l target/release/iaam
-rwxr-xr-x … target/release/iaam
```

**On failure** — `nix: command not found` means the toolchain is not installed on
the build host; supplied by the machine's administrator, or build on another
machine and copy the binary. `--locked` failing means `Cargo.lock` does not
match the manifests: commit the lock file rather than removing the flag.

### 4.2 Install

```console
$ sudo install --mode 0755 --owner root --group root target/release/iaam /usr/local/bin/iaam
$ iaam --help
The iaam service and local administration CLI
…
```

Owned by `root` and not by the service user: the service must not be able to
rewrite the program it runs.

### 4.3 Service user and directories

```console
$ sudo useradd --system --home-dir /var/lib/iaam --shell /usr/sbin/nologin iaam
$ sudo install --directory --owner iaam --group iaam --mode 0700 /var/lib/iaam
$ stat --format '%U %G %a' /var/lib/iaam
iaam iaam 700
```

### 4.4 Claim the instance

```console
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/iaam.db iaam claim --label console
1f0c…  (64 hexadecimal characters, on one line)
```

`sudo -u iaam` is the point of the step: the command must run as the identity
that owns the database, because that identity is the whole of its authority
(§1). Repeating it answers `error: instance is already claimed`, exactly as in
§3.5.

### 4.5 The unit file

```ini
[Unit]
Description=iaam
After=network-online.target
Wants=network-online.target

[Service]
User=iaam
Group=iaam
ExecStart=/usr/local/bin/iaam serve
Environment=IAAM_DATABASE=/var/lib/iaam/iaam.db
Environment=IAAM_LISTEN=127.0.0.1:8080
# The key is delivered as a credential, not as an environment variable (§6.6).
LoadCredential=broker-key:/etc/iaam/broker-key
Environment=IAAM_BROKER_KEY_FILE=%d/broker-key
Restart=on-failure

# Ordinary service confinement. Not specific to the key, but it removes half of
# the ways to reach it.
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
NoNewPrivileges=true
StateDirectory=iaam

[Install]
WantedBy=multi-user.target
```

Write it to `/etc/systemd/system/iaam.service`, then:

```console
$ sudo systemctl daemon-reload
$ sudo systemctl enable --now iaam
$ systemctl is-active iaam
active
$ journalctl -u iaam -n 1 --no-pager
… iaam[…]: INFO iaam: server started address=127.0.0.1:8080
```

Drop the two key lines if this instance has no broker access yet; add them and
`systemctl restart iaam` after §6.1.

**On failure** — `systemctl is-active iaam` printing `failed` means the process
exited; `journalctl -u iaam -n 20 --no-pager` holds the message, and §11 holds
its meaning.

---

## 5. Proof that the deployment works

Three calls, in order. The third is the one that proves it. Replace `$OWNER`
with the token from §3.5 or §4.4; on the container route the port is the one
`--publish` names.

```console
$ curl -sS -i http://127.0.0.1:8080/.well-known/api-catalog
HTTP/1.1 200 OK
content-type: application/linkset+json
…
{"linkset":[{"anchor":"/v1","service-desc":[{"href":"/v1/openapi.json","type":"application/json"}],"status":[{"href":"/v1/health","type":"application/json"}]}]}

$ curl -sS http://127.0.0.1:8080/v1/health
{"status":"ok","schema_version":11,"projection_version":8}

$ curl -sS -H "authorization: Bearer $OWNER" http://127.0.0.1:8080/v1/actions
{"policy_version":1,"items":[{"id":"create_first_account","kind":"create_first_account","category":"blocking","state":"needs_owner_input","reason":"No account exists; create one before portfolio actions can be offered.","required_scope":"owner","target":{"type":"operation","operationId":"create_account","method":"POST","path":"/v1/accounts","requestSchema":"#/components/schemas/CreateAccountRequest","request":{"missing":[{"pointer":"/title","provided_by":"owner"}]}}}]}
```

The first call is the discovery document (RFC 9727) and the entry point for an
arriving agent: it links the machine-readable contract at `/v1/openapi.json` and
the status route. The second proves the process is up; check `"status":"ok"`
rather than the version numbers, which move with the build.

The third is the proof. It authenticated a token that only a console could have
issued, read the store, and resolved the action's address from the routes the
server actually registered — transport, storage and the trust root in one
answer. On a freshly claimed instance the queue holds exactly the item above,
and an agent's work starts there rather than in any document a human maintains.

**On failure** — `{"code":"unauthorized", …}` means the header is missing,
misspelled or carries a revoked token; §7 issues a new one, and only a console
can issue an owner token. `Connection refused` means the service is not
listening where you are asking: on the container route compare `docker port iaam`
with the URL, and see §11 for the `IAAM_LISTEN` case.

---

## 6. Broker access

A broker token grants access to a real account, so the database holds only
ciphertext and the key lives outside the database. `serve` reads the key; only
the console writes credentials.

### 6.1 Create the key

Container route:

```console
$ sudo install --directory --owner 10001 --group 10001 --mode 0700 /etc/iaam
$ docker run --rm \
    --mount type=bind,source=/etc/iaam,target=/etc/iaam \
    --mount type=bind,source=/var/lib/iaam,target=/var/lib/iaam \
    --env IAAM_DATABASE=/var/lib/iaam/iaam.db \
    --env IAAM_BROKER_KEY_FILE=/etc/iaam/broker-key \
    iaam:0.1.0 broker key generate
key created: /etc/iaam/broker-key
$ sudo stat --format '%a' /etc/iaam/broker-key
600
```

Binary route:

```console
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/iaam.db \
      IAAM_BROKER_KEY_FILE=/etc/iaam/broker-key iaam broker key generate
key created: /etc/iaam/broker-key
```

The key is never printed and never returned: what nobody saw cannot be forwarded
or saved in the wrong place. An existing file is never overwritten —

```console
$ … iaam broker key generate
error: key file /etc/iaam/broker-key already exists: overwriting it would make every configured access unreadable
```

— because a new key on top of an old one makes every configured access
undecryptable, silently and permanently.

Create the key **before** the service reads it, and then restart the service so
it picks it up: §3.6 with the key mounted, or `systemctl restart iaam`.

### 6.2 Point the running service at the key

Container route: add to the `docker run` of §3.6, mounting the key read-only
because `serve` only reads it.

```
    --mount type=bind,source=/etc/iaam/broker-key,target=/etc/iaam/broker-key,readonly \
    --env IAAM_BROKER_KEY_FILE=/etc/iaam/broker-key \
```

Check:

```console
$ curl -sS -H "authorization: Bearer $OWNER" http://127.0.0.1:8080/v1/broker-access
[]
```

`[]` is success on an instance with no credentials yet. The failure to look for
is this one:

```console
{"code":"not_configured","message":"broker access encryption is not configured: set IAAM_BROKER_KEY_FILE and restart the server"}
```

It means the running process was started without the key. The fix is the
restart, not a different call.

### 6.3 Provision a credential

The token is read from standard input, never from an argument: the process list
is visible to the whole machine and shell history outlives the session.

```console
$ docker run --rm --interactive \
    --mount type=bind,source=/etc/iaam/broker-key,target=/etc/iaam/broker-key,readonly \
    --mount type=bind,source=/var/lib/iaam,target=/var/lib/iaam \
    --env IAAM_DATABASE=/var/lib/iaam/iaam.db \
    --env IAAM_BROKER_KEY_FILE=/etc/iaam/broker-key \
    iaam:0.1.0 broker access add --broker tinkoff --environment sandbox
paste the broker token and finish input (Ctrl-D):
broker access tinkoff (sandbox) provisioned: f4df6218-…
```

Binary route: the same arguments after
`sudo -u iaam env IAAM_DATABASE=… IAAM_BROKER_KEY_FILE=… iaam`.

`--interactive` is required on the container route; without it there is no
standard input to paste into. `--environment` has no default because tokens
differ between `prod` and `sandbox`, and using the wrong one produces a gateway
rejection whose message does not mention the environment.

The token requested from the broker is **read-only**. The scope is recorded
beside the access and parsed before every call, so a record promising trading
rights is refused rather than used — but the broker is not asked to confirm it.
Issue a trading token and the system will not notice.

Check that the plaintext did not reach the database, the same way the test does:

```console
$ sudo grep -a "<first characters of the token>" /var/lib/iaam/iaam.db && echo LEAK || echo clean
clean
```

### 6.4 Replace a credential

When the broker's token is replaced, do not send the new one over HTTP — no
route accepts it. Replace it across the same console boundary:

```console
$ … iaam broker access rotate --broker tinkoff --environment sandbox
paste the broker token and finish input (Ctrl-D):
broker access tinkoff (sandbox) replaced: f4df6218-…
```

The ciphertext of the active access is updated in place: its identifier and
history survive, the arguments carry only the broker and the environment, and
the plaintext exists only until it is encrypted.

### 6.5 Rotate the key

The command takes two files that already exist: the old key and a new one
created beforehand. It decrypts every `broker_access` row, including revoked
ones, and replaces them in a single transaction.

```console
$ … IAAM_BROKER_KEY_FILE=/etc/iaam/broker-key.next iaam broker key generate
key created: /etc/iaam/broker-key.next
$ … iaam broker key rotate --old /etc/iaam/broker-key --new /etc/iaam/broker-key.next
broker accesses re-encrypted: 1
```

The command replaces no files and deletes none. **Keep a backup of the old key
until the command has succeeded and access has been verified**: on failure the
old rows remain under the old key. Only then point the service at the new file
and restart it, and do not delete the backup until a restore from it has been
confirmed.

Losing the key is not recoverable from the database: it holds ciphertext only,
and a new key does not decrypt old rows. A copy of `IAAM_DATABASE` alone is
therefore not a restore. If the old key is gone, do not create a new one over
the old file and do not promise recovery: new credentials can be provisioned,
old ciphertext cannot be read.

### 6.6 Delivering the key in production

Not through an environment variable: its value is readable in
`/proc/<pid>/environ` by the same user and is inherited by every child process.
On the binary route, systemd credentials are the mechanism, and the unit in §4.5
already uses them. `%d` is the credentials directory — ramfs, mode `0400`, owned
by the service user, invisible to other users, not inherited by children, absent
from the process list. `/etc/iaam/broker-key` itself stays owned by `root` with
mode `0600`; the service reads it through systemd rather than directly.

Where a TPM is present the key need not sit on disk in the clear:

```console
$ sudo systemd-creds encrypt --with-key=host+tpm2 /etc/iaam/broker-key /etc/iaam/broker-key.cred
```

and `SetCredentialEncrypted=broker-key:…` replaces `LoadCredential=` in the
unit. A stolen disk or backup then yields nothing, and restarts stay automatic.
A TPM is optional; without one the variant above works and protects against
theft of the database file just as well. When a TPM appears, one line of the
unit changes and no code does.

### 6.7 What none of this closes

Nothing protects against `root` on a running machine. Root reads process memory,
reads the credentials directory, and failing both can simply ask the service to
call the broker. That is a property of the problem: a program that can decrypt
without a human will decrypt for whoever owns it. The only way to exclude it is
to derive the key from a passphrase entered at every start, which costs
unattended restarts and does not exist here. The damage is bounded by
construction instead: the token has no trading rights.

---

## 7. Tokens

A token is presented as `Authorization: Bearer <token>`. Every issued token is
shown **once**; the database holds only its hash and there is nowhere to show it
from again.

### 7.1 Issue a token for an agent

The owner token issues the rest over the API:

```console
$ curl -sS -X POST http://127.0.0.1:8080/v1/tokens \
    -H "authorization: Bearer $OWNER" -H 'content-type: application/json' \
    -d '{"label": "home agent", "scope": "agent"}'
{"id":"8b0f5714-…","token":"0d69…","label":"home agent","scope":"agent"}
```

| Scope | May |
|---|---|
| `owner` | everything, including token and broker-access administration |
| `agent` | submit events and read |
| `read_only` | read |

The `owner` scope cannot be issued over the API:

```console
$ curl -sS -X POST http://127.0.0.1:8080/v1/tokens -H "authorization: Bearer $OWNER" \
    -H 'content-type: application/json' -d '{"label": "second", "scope": "owner"}'
{"code":"invalid_request","message":"an owner token cannot be issued via the API: the owner is created with `iaam claim --label <label>`","field":"scope","expected":"agent or read_only","actual":"owner"}
```

Otherwise a stolen owner token could immediately copy itself into
indistinguishable duplicates, and revoking the original would change nothing.

Give the issued token to the agent through its host's configuration — an
injected header the model cannot print — and not by pasting it into a
conversation. ADR-0003 §2 draws that line and explains why the distinction is
between the model's context and the host's configuration.

### 7.2 List and revoke

```console
$ curl -sS http://127.0.0.1:8080/v1/tokens -H "authorization: Bearer $OWNER"
[{"id":"9520643a-…","label":"console","scope":"owner","created_at":"…","revoked_at":null}, …]

$ curl -sS -X DELETE http://127.0.0.1:8080/v1/tokens/8b0f5714-… \
    -H "authorization: Bearer $OWNER" -o /dev/null -w 'HTTP %{http_code}\n'
HTTP 204
```

Labels and scopes are listed; tokens and hashes are not, and cannot be — the
hash is all an attacker would need. Revoked tokens stay in the list, because
"when did this token stop working" is a question that needs an answer. A revoked
token is then indistinguishable from an unknown one: both get `401`.

### 7.3 A lost owner token

Recovery is by console, and only by console:

```console
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/iaam.db iaam token issue --label console --scope owner
1f0c…
```

On the container route, the `docker run` form of §3.5 with
`token issue --label console --scope owner` in place of `claim`. The command
takes the single existing owner from the database, prints a new owner token once
and exits without starting a server. On an empty database it refuses:

```console
error: instance has no owner: run `iaam claim --label <label>` first
```

The lost token is **not** revoked by this: revoke it with
`DELETE /v1/tokens/{id}`, or it keeps working.

---

## 8. In front of the service

### 8.1 TLS

The service speaks plain HTTP on the loopback interface. TLS is terminated by a
reverse proxy:

```
iaam.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

Without a proxy the bearer token crosses the network in clear text. That is the
only reason the proxy is mandatory, and it is sufficient.

### 8.2 Rate limiting

The built-in limiter is a fixed window per token inside one process. It protects
against an agent stuck in a loop; it does not protect against distributed load
or an attacker, because its state is not shared between processes and is lost on
restart. A limit at the proxy remains mandatory.

---

## 9. Backup

```console
$ sudo -u iaam sqlite3 /var/lib/iaam/iaam.db ".backup /var/backups/iaam-$(date +%F).db"
$ ls -l /var/backups/
```

A copy of the database file is not a complete backup: it is tied to a schema
version and to a platform. Export the archive bundle regularly — it is portable
and its checksum is verified on import.

The database file contains the entire journal of facts. Handle it like a bank
statement. Back up the broker key separately and to a different place (§6.5); a
backup of the database without the key restores the record but not the access.

---

## 10. Two check modes

```console
$ nix develop -c cargo nextest run --workspace
```

touches no network: parsing is checked against frozen sample responses, and this
is the mode CI runs.

```console
$ IAAM_DATABASE=… IAAM_BROKER_KEY_FILE=… nix develop -c cargo test -p iaam-broker --features sandbox
```

uses the broker's real sandbox gateway and a real configured access. It answers
a different question — is the gateway alive, is the embedded trust anchor still
valid, is the configured access accepted — and it **fails** when no access is
configured, because the mode was requested explicitly and a silent skip would be
a lie told by a green run.

The sandbox does not check the report channel: `GetBrokerReport` exists only on
the production contour. Sandbox and production accesses live side by side,
distinguished by `environment`, with one active access per owner + broker +
environment; the live check takes the sandbox access explicitly and never uses
the production one.

Never pass `--all-features`: it enables the sandbox feature and turns any run
into a trip to the internet.

---

## 11. Failure index

Every message below is printed by the program. The right-hand column says who
supplies what is missing and with which command.

| Message | Meaning | Fix |
|---|---|---|
| `error: variable IAAM_DATABASE is not set; set it (allowed values: database file path)` | no database path given; there is no default | whoever writes the run command: add `--env IAAM_DATABASE=/var/lib/iaam/iaam.db` or `Environment=IAAM_DATABASE=…` |
| `error: variable IAAM_LISTEN is invalid: 8080; allowed values: socket address such as 127.0.0.1:8080` | a port without a host | use `0.0.0.0:8080` in a container, `127.0.0.1:8080` on a host |
| ``error: environment variable IAAM_ISSUE_OWNER_TOKEN was replaced by `iaam token issue` `` | a retired provisioning variable is set (§2.4) | remove it from the unit, profile or compose file and run the subcommand |
| `error: instance is already claimed` | the database already has an owner | expected on a second `claim`; for a new token use `iaam token issue --scope owner` (§7.3) |
| ``error: instance has no owner: run `iaam claim --label <label>` first`` | `token issue` against an empty database | run `iaam claim --label console` (§3.5, §4.4) |
| ``error: key file /etc/iaam/broker-key not found; run `iaam broker key generate` `` | `IAAM_BROKER_KEY_FILE` points at nothing | the owner, at a console: §6.1 |
| `error: key file … already exists: overwriting it would make every configured access unreadable` | `broker key generate` over an existing key | none needed; to change keys use `broker key rotate` (§6.5) |
| `error: key file … exists but is unreadable or has an invalid format` | wrong file, or a damaged key | restore the key from backup. Do **not** create a new one over it |
| `error: SQLite error: unable to open database file: /var/lib/iaam/iaam.db` | the data directory is not writable by the process's uid | the machine's administrator: `sudo chown 10001:10001 /var/lib/iaam` (container) or `sudo chown iaam:iaam /var/lib/iaam` (host) |
| `{"code":"unauthorized", …}` (401) | header missing, or the token is unknown or revoked | §7.1 for an agent token; §7.3 for an owner token |
| `{"code":"not_configured","message":"broker access encryption is not configured: …"}` | the server was started without `IAAM_BROKER_KEY_FILE` | restart it with the key mounted: §6.2 |
| `{"code":"invalid_request","message":"an owner token cannot be issued via the API: …"}` (422) | `scope: owner` requested over HTTP | by design; issue it at the console (§7.3) |
| `Connection refused` from curl | nothing is listening at that address | container: `IAAM_LISTEN` left at the loopback default while publishing a port (§3.6). Host: `systemctl is-active iaam` |

---

## 12. Why there is no compose file

A compose file committed to this repository would be the natural place to write
down the host database path, the key file path and the published address — which
are exactly the run-time inputs that must stay outside it (§2.3). The
`docker run` commands above carry those values as arguments, where they are
visible at the moment somebody chooses them, and no committed file accumulates a
default that nobody meant to publish.

The one-shot administrative commands are the second reason: `claim`,
`token issue` and `broker access add` are interactive, run once, and read a
secret from standard input. `docker compose run` adds a layer of indirection
around each of them and buys nothing, since there is exactly one long-running
service to orchestrate.

An operator who wants compose can write one outside this repository from §3.6;
nothing in the image depends on its absence.
