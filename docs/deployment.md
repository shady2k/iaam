# Deployment

The reader of this document is an agent, and the document is written to be
executed. Every step is a command followed by the output that proves it worked.
A step whose success cannot be observed by running something is not a step and
is not in here. Where a step can fail, the failure names what is missing, who
supplies it, and the command that supplies it.

The instance has one default place for each of its two files, chosen by
the XDG rules: the database at `$XDG_DATA_HOME/iaam/iaam.db`, else
`$HOME/.local/share/iaam/iaam.db`; the broker key at
`$XDG_CONFIG_HOME/iaam/broker-key`, else `$HOME/.config/iaam/broker-key` —
apart from the data on purpose, so copying the data directory does not carry
the key. Two rules keep a database in an expected place from ever looking
like a lost portfolio:

- **Only `iaam claim` creates a database.** Every other command that finds
  no database at the place it resolved refuses, names the place it looked
  at, and creates no file and no directory. An empty database never appears
  silently anywhere.
- **`iaam status` says where the instance is**: the database place and the
  key place, whether each file exists, and for each whether the place came
  from a variable or is the default.

A relative `XDG_*` value counts as unset, as the XDG spec says, and so does
an empty or relative `HOME`. An override variable (`IAAM_DATABASE`,
`IAAM_BROKER_KEY_FILE`) set to an empty value is refused, not treated as
unset: a setting that was silently ignored looks exactly like one that was
never read. The key place is asked for only by the commands that need the
key (`iaam broker key …`, `iaam broker access …`, `iaam serve`), so `iaam
claim` and `iaam status` work where only the database is named — `iaam
status` reports the key place and, when it has no place at all, says why
instead of failing.

The image tag `iaam:0.1.0` and the container name `iaam` are still literal
values that the commands below actually pass. A service or a container still
passes explicit paths: inside
a container there is no home directory worth a default, and a service names
its paths so the unit file is the whole truth about where the data is. On
the owner's own console no path is typed at all.

---

## 1. What is deployed

`crates/iaam-bootstrap` builds one binary, `iaam`. It is the entire deployable
and it has two roles.

| Role | Command | Run by |
|---|---|---|
| HTTP service | `iaam serve` | a service manager, unattended |
| local administration | `iaam status`, `iaam claim`, `iaam token issue`, `iaam broker key …`, `iaam broker access …`, `iaam bundle export`, `iaam bundle import` | the owner, at a console |

The second role is not a convenience wrapper. Under
[ADR-0003](decisions/0003-the-owner-speaks-to-an-agent-and-a-cli-keeps-the-secrets.md)
the CLI owns the trust root and every secret: **no HTTP route issues an owner
token, and no HTTP route accepts a broker credential.** The CLI's authority is
the operating system's — the identity it runs as, the permissions on the
database and key files, and the boundary that decides who may execute it at all.
A deployment where anyone can run `iaam claim` against the database file has
given away ownership of the instance; §3.4 and §4.4 are where that boundary is
actually set.

`iaam bundle export` and `iaam bundle import` join this list for a different
reason than the rest: they carry no secret (a bundle never contains a token or
a broker credential — `crates/iaam-bootstrap/src/bundle.rs` names every table
that stays out). What they share with the rest of the row is where the file
lands: an export is the owner's entire financial history in one file, and
writing it is a console act with a chosen path and no HTTP body, proxy or
access log between the database and the disk — see that same doc comment for
the full reasoning against a route.

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

### 1.1 One server process and one owner per broker endpoint

An instance runs **exactly one `iaam serve`** against its database. Not two
behind a load balancer, not a second one started beside the first "to test",
not a blue-green overlap where the old one keeps serving while the new one
starts. The claim that permits only one broker sync per account at a time is
in-process state (`crates/iaam-app/src/scenarios/sync.rs`, `RunningSyncs`), so a
second server could sync the same account concurrently.

The outbound gateway also keeps its MOEX and CBR lanes, named waits and circuit
breakers in the server process. They do not cross a restart.

Broker traffic has a stronger rule: **one process owns each broker endpoint**.
The first request to T-Invest production, T-Invest sandbox or Finam acquires
`tinkoff-prod.owner`, `tinkoff-sandbox.owner` or `finam.owner` in the
instance's egress directory — derived from the database path, see §2.1 — and
holds that lock for the gateway's lifetime. A second
process asking for that endpoint is refused before transport; the reason names
the endpoint and tells the operator to use or stop the owning process. It may
still own and use another endpoint.
This makes accidental concurrent servers observable instead of trying to share
one broker allowance between them.

Within an owned endpoint, the gateway allows one request in flight. The HTTP
client follows no redirect and performs no retry below the gateway. It commits
the reservation at transport handoff, then records the status line and a
Retry-After clamped to one day before consuming the body. The method, rolling
day and one-second spacing timestamps all move to the later of the original
decision and the handoff or status commit; delayed handoff cannot age an
allowance before a byte is sent. The next request cannot decide before that
record exists. If the detached transport/status task panics, its lane records
that failure before unlocking; the next lane holder durably closes the endpoint
for one hour before deciding, even when the status line was already recorded.
The shared `outbound-tally.lock` is held only for a tally transaction; the boot clock is
sampled while this lock is held, and waits and HTTP requests do not hold it. The
tally preserves:

- the per-method minute budgets from the gateway table;
- at least one second between sends to the same broker host;
- at most 1,000 sends per endpoint in any rolling 24-hour interval;
- recent permanent broker refusals and their 30-minute host closure;
- broker `429` pauses of at least 60 seconds and the 30-minute closure after a
  second `429` in ten minutes.

The persisted time source is Linux boot identity plus `CLOCK_BOOTTIME`, not
wall time. Wall-clock steps therefore cannot shorten a pause, closure, spacing
window or rolling daily window. After a boot identity change, iaam cannot know
how much suspended time elapsed before the reboot: every active pause restarts
for its stored duration; every closure restarts from its persisted reason
(30 minutes for repeated broker refusals or rate limits, one hour for an
unresolved attempt); old request histories restart from the new boot; and the
first send to each endpoint waits 60 seconds.

The gateway places the instance's egress directory beside the instance's
database (`iaam.sqlite` → `iaam.sqlite.egress`, canonicalized first: every
symlink or relative alias of the same database reaches the same directory, and a
database file with a second hard-linked name is refused, because each name would
get its own tally) and opens it when broker
egress is enabled, keeping that directory descriptor for its lifetime. One
database is one tally: this is the per-instance guarantee, and no separate
setting exists that could split one instance's tally. When the directory is
missing, the process creates it with mode 0700 and the two fresh tally records
in it — no step needs root. The
`outbound-tally`, `outbound-tally-generation`, lock, owner and temporary records
are opened relative to the descriptor without following symlinks, then checked
again by device and inode. The tally and its separate generation record advance
together; an emptied tally or a generation mismatch refuses operation instead
of accepting truncation or rollback. A missing, unreadable, replaced,
symlinked, hard-linked or corrupt record refuses broker operation; there is no
in-memory allowance fallback.

Run the executable ceiling proof before enabling broker egress:

```console
$ make ceiling-proof
```

The proof sends through the production gateway and HTTP client to loopback TCP
servers, one endpoint at a time. The receiver records completed request headers,
so the table is derived from wire arrivals rather than gateway counters.
`sec(logical)` is checked for every row from the injected boot clock.
`sec(wire)` is measured with a dedicated real-time spacing run for every
endpoint; other rows say `wire-row` and rely on that endpoint's named spacing
row instead of presenting logical time as wire time. The remaining duration
columns are `reached/ceiling` from the injected boot clock sampled when each
request reaches the receiver. `minute(method)` names the endpoint-method pair
whose rolling 60-second window was largest. Unknown or near-miss paths are
refused before transport. `closure` and `pause` must both be `0/0`; `attempts`
includes retries and Finam session exchanges.

The command also exercises the real T-Invest and Finam sync loops; retry and
response-body failures; redirect policy; exact Retry-After, 30-minute closure,
90-second unresolved-send and rolling 24-hour boundaries; concurrent callers;
caller cancellation, deadline expiry, a panic after the status line, a killed
child, and a durable status-commit failure; a clock advance between reservation
and transport handoff; boot identity changes; tally persistence across owner
rebuilds; egress-off; path aliases; and two-process ownership. The process rows
are reconstructed from the loopback receiver's actual wire-arrival records.
The two child processes deliberately disagree in an irrelevant environment
variable while receiving the same database path, proving that the tally beside
the database, not process-local environment, coordinates ownership. A separate
row builds gateways over aliases of one database — a symlink to the file, a
symlink to its directory, a redundant `..`, a relative path — and shows they
share one tally and one endpoint owner, while a second database keeps a second
tally: two instances with two databases have two tallies.

There is deliberately no fake wall-clock-step row. The production
`Clock`/`SystemClock` used for persisted ceilings exposes only in-process
monotonic time and Linux boot time; it has no wall-clock read to step. The proof
does change the boot identity and checks the conservative reset behavior.

The proof deliberately does not contact live brokers or test TLS, proxies,
network filesystems, separate machines, container mount namespaces, or a
hostile process modifying the tally. The loopback fixture speaks HTTP/1.1 on
Linux. Its real-time spacing rows observe the host scheduler; longer accounting
windows are advanced by the injected boot clock so the check remains bounded
and deterministic.

Administrative commands (`claim`, `token issue`, `broker key …`,
`broker access …`, `bundle export`, `bundle import`) open the database and do
not contact a source. The two broker examples and the ignored live sandbox
test do contact brokers and consequently use the same egress switch, endpoint
ownership and tally as `serve`.

```console
$ pgrep -c -x iaam
1
```

More than `1` while no administrative command is running means a second server.
Stop it before the next sync. Restarting does not clear the broker tally and
must not be used to evade a `source_unavailable`; in-process named waits and
open breakers are still lost on restart.

---

## 2. Configuration

### 2.1 Every variable the program reads

| Variable | Kind | Default | Read by |
|---|---|---|---|
| `IAAM_DATABASE` | path to the database | `$XDG_DATA_HOME/iaam/iaam.db`, else `$HOME/.local/share/iaam/iaam.db` | every command, including `serve` |
| `IAAM_BROKER_KEY_FILE` | path to a secret | `$XDG_CONFIG_HOME/iaam/broker-key`, else `$HOME/.config/iaam/broker-key` | `broker key generate`, `broker connect`, `broker access add`, `broker access rotate`; optional for `serve` |
| `IAAM_BROKER_EGRESS` | `off` or `on` | the instance's stored switch (§6.8): on after `iaam broker connect`, off otherwise; `off` here still forces broker requests off | `serve` reads it over the stored switch; developer tools may set `on` |
| the egress directory | directory beside the database (`<database>.egress`), created by the process when missing | derived from `IAAM_DATABASE` — no variable and no override | `serve`, broker examples, ignored live sandbox test |
| `IAAM_LISTEN` | optional | `127.0.0.1:8080` | `serve` |
| `IAAM_RATE_LIMIT` | optional | `120` | `serve` |
| `IAAM_RATE_WINDOW_SECONDS` | optional | `60` | `serve` |
| `IAAM_SOURCE_PROFILES` | path to a read-only directory | none | `serve` |
| `RUST_LOG` | optional | `info` | `serve` |

With no `HOME` and no override, a command refuses and names the variables
that would supply the place:

```console
$ env -u HOME -u XDG_DATA_HOME -u XDG_CONFIG_HOME iaam status
error: no place for the instance's database: set IAAM_DATABASE, or XDG_DATA_HOME, or HOME; none of them is set
$ echo $?
1
```

The short forms need no variable at all. `iaam status` first, then the one
creating command, then the refusal that proves every other command refuses
rather than create:

```console
$ iaam status
database: /home/dev/.local/share/iaam/iaam.db (default; absent)
broker key: /home/dev/.config/iaam/broker-key (default; absent)
$ iaam token issue owner
error: no database at /home/dev/.local/share/iaam/iaam.db: a database is created only by `iaam claim`, no other command creates one
$ iaam claim --label console
1f0c…  (64 hexadecimal characters, on one line)
$ iaam status
database: /home/dev/.local/share/iaam/iaam.db (default; present)
broker key: /home/dev/.config/iaam/broker-key (default; absent)
```

`IAAM_DATABASE` and `IAAM_BROKER_KEY_FILE` still override the places when a
deployment names them; every example below that passes them is a service or
a container, which name their paths explicitly.

`IAAM_BROKER_KEY_FILE` is optional for `serve` only in the sense that a service
that never talks to a broker can run without it. `serve` reads the key from
the resolved place when a file is there. When the place was named by the
variable and the file is absent, `serve` refuses to start rather than
starting silently without encryption; when only the default place is empty,
it starts without encryption. Broker routes that **use** a credential — a
sync, anything that decrypts — answer `{"code":"not_configured", …}` on a
server started without a key, and the fix is a restart with the key, not a
different call.
`GET /v1/broker-access` is not one of them: it lists metadata, decrypts nothing,
and answers `200` with or without the key (§6.2).

The instance's broker requests are governed by **a stored switch**
(migration `0008`, read with `iaam status`). `iaam broker connect` turns it
on as part of connecting; `iaam broker off` turns it off. `serve` reads the
stored switch at start. The `IAAM_BROKER_EGRESS` variable still overrides it:
`off` forces a deployment that must not call a broker to stay silent, and
`on` stays accepted as an override for developer tools. Unset, the stored
word stands; an invalid value is refused, as before.

Broker requests still refuse — before the tally or the network is touched —
whenever the effective switch is off, and the tally directory is still
derived from the instance's database (§2.1, `IAAM_DATABASE`); there is no
separate path override, so one instance cannot be given a second tally.

No preparation step exists: when the directory or the two fresh tally records
are missing, the process creates them (mode 0700 on the directory) beside the
database before opening them.

**The first enabling mints a fresh zero tally.** On the instance's first
`broker connect` — while the database records no first enabling — iaam
writes the pair beside the database not as the empty pair below but as a
fresh zero pair: current format, current boot, matched generation, no
recorded attempt anywhere. Every ceiling still holds and nothing is spent,
so the connect command's one checking call goes out at once instead of
sitting out the conservative day. The database records that first enabling,
and from then on the rule below applies exactly as before: a deleted or
emptied egress directory restores nothing, because the database vouches only
for the pair it watched being minted. Deleting the directory after the first
enabling spends the day, exactly as emptying it always did.

Every iaam process of one instance must reach the same database — and thereby
the same egress directory — and run as an OS user able to read, write and sync
both tally records; create and lock its lock and three endpoint-owner records;
create its temporary records; atomically rename within the directory; and sync
the directory. Two instances with two databases have two tallies, and that is
the guarantee: an instance is one tally, not a machine. Processes on
different machines do not share this coordination. Network filesystems and
cross-machine tally sharing are outside the proof.

A pair of empty records — including the fresh pair the process itself creates
when the place is missing, and **excepting only the fresh zero pair the first
enabling itself writes (above)** — is accepted only
as a conservative recovery state: on first use iaam records 1,000 attempts at
the current time for **each** broker endpoint, so every broker endpoint remains
at its rolling-day ceiling for 24 hours. This is deliberate; an empty pair
cannot prove that an earlier tally was unused. Only the current
`iaam-outbound-tally-v4` format is accepted; no older format was deployed, so
any older header is corruption, not a migration source. The two records carry
the same generation. A process also remembers the highest matching generation
it has read and refuses a later matching pair below that high-water mark, even
if both files were rolled back together. Leftover temporary files are ignored;
only the last complete tally and matching generation are read.

Do not copy, alias, replace, delete or truncate a live egress directory or any
record in it. The process verifies the path and held inodes before each
transaction. A reservation becomes pending before network I/O and is cleared
only after the response status is durably committed. Cancellation, deadline,
transport failure or panic moves the reservation to the observation time before
clearing it and closes the endpoint for the request timeout plus the mandatory
60-second rate-limit pause: 90 seconds. Pruning retains the timestamps that
belong to a pending attempt. Process death leaves the pending marker in place;
a new owner that adopts it reinserts any timestamps lost by an older pruner at
its own acquisition time and closes that endpoint for a full hour from
adoption. Other endpoint activity and the original handoff's age do not shorten
that closure.

Operational repair is a controlled stop: stop every process using the
directory and
repair both records as one matched pair. If no trustworthy matched pair exists,
empty both records together, start one process, and expect the documented
24-hour daily-ceiling refusal on every broker endpoint. After that interval,
run `make ceiling-proof` before restoring traffic. Never restore just one
record or bypass the conservative interval by editing timestamps or generation
numbers.


`IAAM_SOURCE_PROFILES` names a directory of **source profiles** — reviewed JSON
files describing one institution's export, which the server reads a document
through (`POST /v1/import-sessions/{session}/document`). It has no default and
cannot have one: a profile decides how every future row of that format is read,
and one picked up from a known place would be one nobody chose. Unset, the
server reads with the profiles the image ships, which is a complete catalogue
and not a degraded one.

The directory is read **once, at start-up**, and only its `.json` files are
considered; mount it read-only. Nothing is loaded into the server as code — a
profile is data, it names columns and translates the source's own words, and it
computes nothing. A file that is not a valid profile does not stop the server
and does not take the other formats down with it: it is **published as refused**
by `GET /v1/source-profiles`, with the place in the file and what was wrong. Read
that list after changing the directory. A profile that merely failed to load
looks exactly like one nobody wrote, and the symptom a month later is an export
answered "no profile recognises this document".

A local profile whose `id` matches one the image ships does **not** shadow it: it
is refused, and published as refused, for the same reason.

### 2.2 Secrets

Never in the image, never in a file committed to this repository, never in a
conversation.

| Secret | Where it lives | How it is created |
|---|---|---|
| broker encryption key | a file outside the database, mode `0600`, at `$XDG_CONFIG_HOME/iaam/broker-key`, else `$HOME/.config/iaam/broker-key`; `IAAM_BROKER_KEY_FILE` names a different place. The key lives apart from the data on purpose: copying the data directory does not carry the key | `iaam broker key generate` (§6.1) |
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
$ IAAM_DATABASE=/var/lib/iaam/iaam.db IAAM_ISSUE_OWNER_TOKEN=console iaam token issue owner
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
  status  Show where the instance's database and broker key live
  claim   Claim a fresh instance and print its owner token once
  token   Manage API tokens
  broker  Manage broker credentials and access
  bundle  Move an instance's transferable state in and out of a file (§14)
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
life of a database. `--label` names the token for `iaam token list`; without
it the token is named `owner`.

```console
$ docker run --rm \
    --mount type=bind,source=/var/lib/iaam,target=/var/lib/iaam \
    --env IAAM_DATABASE=/var/lib/iaam/iaam.db \
    iaam:0.1.0 claim
c35d72df…  (64 hexadecimal characters, on one line)
shown only now: put it in the owner's password manager or the agent's configuration; it cannot be shown again
```

Record it in the operator's password manager now. Then check that the claim took
effect, by making it a second time:

```console
$ docker run --rm --mount type=bind,source=/var/lib/iaam,target=/var/lib/iaam \
    --env IAAM_DATABASE=/var/lib/iaam/iaam.db iaam:0.1.0 claim
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
uid cannot write to it. `error: no place for the instance's database: set
IAAM_DATABASE, …` is a missing `--env`: the image sets no `HOME`, so inside
a container there is no default place. Supplied by whoever writes the run
command.

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
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/iaam.db iaam claim
c35d72df…  (64 hexadecimal characters, on one line)
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
{"linkset":[{"anchor":"/v1","related":[{"href":"/v1/actions",…},{"href":"/v1/contours",…},{"goal":"asset_snapshot","href":"/v1/reports/assets",…},{"goal":"money_flow","href":"/v1/reports/flow",…},{"goal":"returns","href":"/v1/reports/returns",…},{"goal":"reconciliation","href":"/v1/reconciliation",…}],"service-desc":[{"href":"/v1/openapi.json",…}],"status":[{"href":"/v1/health",…}]}]}

$ curl -sS http://127.0.0.1:8080/v1/health
{"status":"ok","schema_version":12,"projection_version":8}

$ curl -sS -H "authorization: Bearer $OWNER" http://127.0.0.1:8080/v1/actions
{"items":[{"id":"create_first_account","kind":"create_first_account","category":"blocking","goals":[],"state":"needs_owner_input","reason":"No account exists; create one before portfolio actions can be offered. Which accounts to create is a question this instance answers rather than guesses at, …","required_scope":"owner","target":{"type":"operation","operationId":"create_account","method":"POST","path":"/v1/accounts","requestSchema":"#/components/schemas/CreateAccountRequest","requiredScope":"owner","request":{"missing":[{"pointer":"/title","provided_by":"owner"}]}}}],"reports":[{"goal":"asset_snapshot","answers":"What the owner holds at a date: cash and positions, and the whole.","blocked_by":["create_first_account"],"answered_by":{"operationId":"asset_snapshot_report","method":"GET","path":"/v1/reports/assets","requiredScope":"read_only"}},{"goal":"money_flow","answers":"Where money came from and where it went, over an interval.","blocked_by":["create_first_account"],"answered_by":{"operationId":"flow_report","method":"GET","path":"/v1/reports/flow","requiredScope":"read_only"}},{"goal":"returns","answers":"What the money earned, before tax.","blocked_by":["create_first_account"],"answered_by":{"operationId":"returns_report","method":"GET","path":"/v1/reports/returns","requiredScope":"read_only"}},{"goal":"reconciliation","answers":"Whether the journal agrees with what the sources say.","blocked_by":["create_first_account"],"answered_by":{"operationId":"reconciliation","method":"GET","path":"/v1/reconciliation","requiredScope":"read_only"}}]}
```

The first call is the discovery document (RFC 9727) and the entry point for an
arriving agent. It links the machine-readable contract at `/v1/openapi.json` and
the status route, and beside them the ordering the contract cannot express: the
outstanding-work queue, the scopes every report takes an id from, and the route
answering each of the four goals, each tagged with the goal name the reports and
the queue use. Every `title` is elided above; every address is resolved from the
generated contract when the process starts, so a link here cannot outlive the
route it names. The second proves the process is up; check `"status":"ok"`
rather than the version numbers, which move with the build.

The third is the proof. It authenticated a token that only a console could have
issued, read the store, and resolved the action's address from the routes the
server actually registered — transport, storage and the trust root in one
answer. On a freshly claimed instance the queue holds exactly the item above,
and an agent's work starts there rather than in any document a human maintains.

The queue is under `items`. It was a bare array for a while — the wrapper it
had carried a `policy_version` that turned out to be a literal nothing ever
moved — and it is an object again for a fact that is genuinely about the whole
answer rather than about any item; `docs/api/conventions.md` §1.4a records both
turns and the rule they follow from.

`goals` is empty on this item because it is `blocking` — it stops the next call
rather than standing between the owner and a report. On an item graded
`required_for_goal` it names which of the four reports the item blocks, in the
same words the reports themselves use, so "what is in the way of the asset
snapshot" is a filter on this list rather than a reading of the whole queue.

`reports` is that mapping folded the other way, and it is why the response is an
object: which reports are answerable right now is said, for an answerable one, by
no item appearing — and no item can state that it is absent. Each of the four
carries what it answers, in a sentence a person can be read, and the identity of
every item standing in its way, most urgent first. An empty `blocked_by` says
nothing outstanding stands in the way of that report. It does not say the report
is complete: a report states what it is silent or partial about in its own
confidence register, which this queue does not read. Each standing names that
register's own call in `answered_by` — resolved from the contract at start-up,
and `read_only` because a report demands no write authority — so a caller told
that nothing stands in the way has the address to go and read what the report
still says about itself. Here all four are held up by
the one blocking item, which is what a freshly claimed instance should say — it
holds no account, no scope and no fact.

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

§6.8 is the one command the owner runs to connect a broker. The rest of §6 is
its anatomy, one step at a time: the key, the service's view of the key, the
stored credential, its replacement, the key's rotation, and how the key is
delivered in production.

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

Check that the route answers:

```console
$ curl -sS -H "authorization: Bearer $OWNER" http://127.0.0.1:8080/v1/broker-access
[]
```

`[]` is an instance with no credentials yet, and that is the whole of what this
call proves. It lists metadata and decrypts nothing, so it answers `200` with or
without the key, and once §6.3 has provisioned an access it prints the row on a
server that cannot read it. **It is not the check that the mount above took
effect.** The call that needs the key is one that uses a credential, so the
proof comes after §6.3:

```console
$ curl -sS -X POST http://127.0.0.1:8080/v1/brokers/tinkoff/sync \
    -H "authorization: Bearer $OWNER" -H 'content-type: application/json' \
    -d '{"account":"<account id>","from":"2025-01-01","to":"2025-01-31"}'
{"code":"not_configured","message":"broker access encryption is not configured: set IAAM_BROKER_KEY_FILE and restart the server"}
```

`503` with that message means the running process was started without the key.
The fix is the restart, not a different call. Its near neighbour
`{"code":"not_configured","message":"broker access is not configured"}` shares
the code and says something else entirely — no active access exists for that
broker and environment — and §6.3 answers it, not a restart.

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

### 6.8 Connect a broker with one command

`iaam broker connect` is the one command the owner remembers. It finds the
instance by the default places, creates the encryption key when there is
none, asks for the token on the terminal without echoing it, makes **one**
read-only call through the gateway to check it — the same tally and ceilings
as every broker request, with the fresh zero pair of §2.1 minted first on the
instance's first enabling — and only after the broker answered does it store
the credential (the §6.4 path) and turn the stored switch on. Finam has only
production; T-Invest takes `--sandbox` for its sandbox:

```console
$ iaam broker connect finam
paste the finam token for the prod environment and press Enter (it stays hidden):
Finam connected: the token sees 2 accounts. Broker requests are on; turn them off with `iaam broker off`.
```

That success line is **described, not run**: running it calls the real
broker, and no real broker is called for this document. The number is the
count of accounts the token sees at the broker, read from the check call's
own answer.

Until the instance exists, the command refuses exactly as every command
does, and creates nothing:

```console
$ iaam broker connect finam
error: no database at /home/dev/.local/share/iaam/iaam.db: a database is created only by `iaam claim`, no other command creates one
$ iaam broker off
error: no database at /home/dev/.local/share/iaam/iaam.db: a database is created only by `iaam claim`, no other command creates one
```

After `iaam claim`, an empty paste is refused before anything is sent, and
`iaam status` carries the broker lines — whether requests are on and which
brokers are connected, names and environments only, never secrets:

```console
$ iaam broker connect finam
paste the finam token for the prod environment and press Enter (it stays hidden):
error: the token is empty: paste the token from the broker's own token page and press Enter
$ iaam broker off
broker requests are off: `serve` will not send them; `iaam broker connect <broker>` turns them on again, and IAAM_BROKER_EGRESS=on still forces them on for developer tools.
$ iaam status
database: /home/dev/.local/share/iaam/iaam.db (default; present)
broker key: /home/dev/.config/iaam/broker-key (default; absent)
broker requests: off
connected brokers: none
```

The broker's own refusals are the command's contract, and they are the lines
**not run** here, for the same reason:

- the broker refuses the token (a 401 or 403, or the broker's own token
  error): `Finam refused the token. Nothing was stored and the switch is as
  it was: check the token for this environment and run `iaam broker connect
  finam` again.` — the token is never named and never printed;
- the broker is paused or closed: the line names when the endpoint reopens
  and nothing is stored;
- the network is unreachable: the line says how many attempts were made and
  nothing is stored.

An active credential for the same broker and environment refuses the command
until `--replace` is passed; `--replace` runs the same check and then
replaces the credential in place (§6.4), so a bad new token cannot destroy a
working one.

The check goes out through the process's one gateway, so it spends the same
allowance as every broker request — on the instance's first enabling from
the fresh zero pair, and afterwards from whatever the pair beside the
database holds. Deleting or emptying that pair after the first enabling
spends the day (§2.1); the command then refuses with the ceiling it finds,
and nothing restores the allowance.

---

## 7. Tokens

A token is presented as `Authorization: Bearer <token>`. Every issued token is
shown **once**; the database holds only its hash and there is nowhere to show it
from again.

### 7.1 Issue a token for an agent

From the console — the scope is the positional argument, `--label` is
optional and defaults to the scope and the day (`agent 2026-09-30`):

```console
$ iaam token issue agent
0252baae…  (64 hexadecimal characters, on one line)
shown only now: put it in the owner's password manager or the agent's configuration; it cannot be shown again
```

Over the API, the owner token issues the rest:

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
{"code":"invalid_request","message":"an owner token cannot be issued via the API: the owner is created with `iaam claim --label <label>`","field":"scope","pointer":"/scope","expected":"agent or read_only","actual":"owner","alternatives":[{"value":"agent"},{"value":"read_only"}]}
```

Otherwise a stolen owner token could immediately copy itself into
indistinguishable duplicates, and revoking the original would change nothing.

Give the issued token to the agent through its host's configuration — an
injected header the model cannot print — and not by pasting it into a
conversation. ADR-0003 §2 draws that line and explains why the distinction is
between the model's context and the host's configuration.

### 7.2 List and revoke

From the console: one line per token — the id, the label, the scope, when it
was created, and `active` or `revoked <time>` — active first, then the
revoked ones. The secret and its hash are never in the list:

```console
$ iaam token list
e8873921-246f-4724-b457-821f481d2669  owner  owner  created 2026-09-30T08:33:53.696868051Z  active
de87122d-718e-4c70-8395-0778dab631f3  read-only 2026-09-30  read-only  created 2026-09-30T08:33:53.780679107Z  active
4987c35e-576f-48e9-a5fa-0841c938ca81  agent 2026-09-30  agent  created 2026-09-30T08:33:53.751428155Z  revoked 2026-09-30T08:33:53.835333474Z

$ iaam token revoke "agent 2026-09-30"
revoked: agent 2026-09-30 (agent, id 4987c35e-576f-48e9-a5fa-0841c938ca81)
```

A label naming two active tokens (the default label is the scope and the
day) is refused with their ids, and the owner revokes one by id:

```console
$ iaam token revoke "agent 2026-09-30"
error: label "agent 2026-09-30" names 2 active tokens; revoke one by its id:
  4cf1035d-d3fb-4800-9865-8a62de55a990  created 2026-09-30T08:34:04.545094219Z
  796af525-d7af-4df2-9fa4-8566f6feedd9  created 2026-09-30T08:34:04.577454703Z
$ iaam token revoke 4cf1035d-d3fb-4800-9865-8a62de55a990
revoked: agent 2026-09-30 (agent, id 4cf1035d-d3fb-4800-9865-8a62de55a990)
```

The same two acts over the API:

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
Revoking from the console takes effect at once: the next request with the
token is refused.

### 7.3 A lost owner token

Recovery is by console, and only by console:

```console
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/iaam.db iaam token issue owner
1f0c…
```

On the container route, the `docker run` form of §3.5 with
`token issue owner` in place of `claim`. The command
takes the single existing owner from the database, prints a new owner token once
and exits without starting a server. On a database with no owner it refuses:

```console
error: instance has no owner: run `iaam claim` first
```

The lost token is **not** revoked by this: revoke it with
`iaam token revoke <label-or-id>` (or `DELETE /v1/tokens/{id}` over the
API), or it keeps working.

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
version and to a platform. Export the archive bundle regularly instead — it is
portable and its checksum is verified on import:

```console
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/iaam.db \
    iaam bundle export --output /var/backups/iaam-$(date +%F).bundle.json
wrote /var/backups/iaam-2026-09-10.bundle.json (bundle format 3, schema 2, exported 2026-09-10T12:00:00Z, 812 events)
```

It refuses to overwrite a file that already exists, so a mistyped path fails
loudly rather than replacing yesterday's archive.

Restoring is the same command in reverse:

```console
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/new-iaam.db \
    iaam claim --label console
$ sudo -u iaam env IAAM_DATABASE=/var/lib/iaam/new-iaam.db \
    iaam bundle import --input /var/backups/iaam-2026-09-10.bundle.json
restored /var/backups/iaam-2026-09-10.bundle.json (bundle format 3, schema 2, exported 2026-09-10T12:00:00Z)
archive was recorded under owner 1a85610c-…; tokens do not travel in a bundle, so it is attached to this instance's own owner 5e2f…-… instead
events: 812 written, 0 already recorded
not restored — 27 table(s) this build's schema holds do not travel in a bundle:
  instrument_aliases: not yet carried; tracked as iaam-k3gh.9.3
  … (evidence the owner acquired and cannot re-fetch, still being added table by table)
  snapshots: recomputed from the journal on restore, not carried
  schedule_snapshots: recomputed from the journal on restore, not carried
  schedule_completeness: recomputed from the journal on restore, not carried
  api_tokens: a credential; never copied into a portable file
  token_usage: a credential; never copied into a portable file
  broker_access: a credential; never copied into a portable file
```

The count and the list are read from the running build's own schema
(`iaam_store::bundle::TABLE_DISPOSITIONS`), not typed in by this document, so
it grows or shrinks exactly as the schema and the bundle's coverage of it do —
what is missing today is what the report above says is missing, never less.

`iaam claim` runs first because a restore attaches the archive's facts to
*this* instance's owner, not the exporting instance's — no token from the old
instance would ever be reissued under a different identifier, so the report
says so rather than leaving a restored instance nobody's token can read.
Restoring into a database that already holds journal facts is refused unless
`--merge` is passed explicitly (`error: this instance already holds journal
facts; …`): there is no default that quietly decides between "start fresh" and
"add to what is here."

The database file contains the entire journal of facts. Handle it like a bank
statement — the same is true of a bundle exported from it, and more so: it is
a single file meant to be copied, so keep it exactly as guarded as the database
itself. Back up the broker key separately and to a different place (§6.5); a
backup of the database, or a bundle export, never carries the key or the
broker access it protects — restoring one restores the record but not the
access.

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
| `error: no place for the instance's database: set IAAM_DATABASE, or XDG_DATA_HOME, or HOME; none of them is set` | no database path given and no home to hang the default place on — a bare container has no `HOME` | whoever writes the run command: add `--env IAAM_DATABASE=/var/lib/iaam/iaam.db` or `Environment=IAAM_DATABASE=…` |
| ``error: no database at <path>: a database is created only by `iaam claim`, no other command creates one`` | the resolved place holds no database, and the command created nothing there | the owner, at a console: `iaam claim` (§2.1, §3.5, §4.4) |
| `error: variable IAAM_LISTEN is invalid: 8080; allowed values: socket address such as 127.0.0.1:8080` | a port without a host | use `0.0.0.0:8080` in a container, `127.0.0.1:8080` on a host |
| ``error: environment variable IAAM_ISSUE_OWNER_TOKEN was replaced by `iaam token issue` `` | a retired provisioning variable is set (§2.4) | remove it from the unit, profile or compose file and run the subcommand |
| `error: instance is already claimed` | the database already has an owner | expected on a second `claim`; for a new token use `iaam token issue owner` (§7.3) |
| ``error: instance has no owner: run `iaam claim` first`` | `token issue`, or `bundle export`/`bundle import`, against an empty database | run `iaam claim` (§3.5, §4.4) |
| `error: multiple owners recorded in the database: …` | `bundle export`/`bundle import` against a database with more than one owner | inspect the database; this is corruption in a single-owner system, not something the command guesses past |
| `error: this instance already holds journal facts; restoring would merge the archive into them …` | `bundle import` against a database that is not empty, without `--merge` | pass `--merge` if merging is what is wanted (§9); otherwise restore into an empty database |
| `error: … already exists: refusing to overwrite an existing archive` | `bundle export --output` names a file that is already there | choose a new path, or move the existing archive aside first |
| ``error: key file /etc/iaam/broker-key not found; run `iaam broker key generate` `` | `IAAM_BROKER_KEY_FILE` points at nothing | the owner, at a console: §6.1 |
| `error: key file … already exists: overwriting it would make every configured access unreadable` | `broker key generate` over an existing key | none needed; to change keys use `broker key rotate` (§6.5) |
| `error: key file … exists but is unreadable or has an invalid format` | wrong file, or a damaged key | restore the key from backup. Do **not** create a new one over it |
| `error: SQLite error: unable to open database file: /var/lib/iaam/iaam.db` | the data directory is not writable by the process's uid | the machine's administrator: `sudo chown 10001:10001 /var/lib/iaam` (container) or `sudo chown iaam:iaam /var/lib/iaam` (host) |
| `{"code":"unauthorized", …}` (401) | header missing, or the token is unknown or revoked | §7.1 for an agent token; §7.3 for an owner token |
| `{"code":"not_configured","message":"broker access encryption is not configured: …"}` (503) | the server was started without `IAAM_BROKER_KEY_FILE` | restart it with the key mounted: §6.2 |
| `{"code":"not_configured","message":"broker access is not configured"}` (503) | same code, different fact: no active access for that broker and environment | the owner, at a console: `iaam broker access add` (§6.3). A restart changes nothing |
| `{"code":"source_unavailable", …}` (503, with `Retry-After` when the wait is known) | an outside source — the broker on `POST /v1/brokers/{broker}/sync`, MOEX or the CBR on `POST /v1/market/sync` — could not be reached for now. A broker `429` immediately persists a host pause of at least 60 seconds; a second within ten minutes closes that host for 30 minutes. Three other broker `4xx` responses within ten minutes also close it for 30 minutes. A broker `5xx`, network failure or timeout gets at most three attempts per call; market sources retain five. The in-process breaker opens after five whole calls fail transiently, and a call during its cool-down is not sent. A source may also name a wait longer than the gateway holds a call (15 minutes), a wait may outlast the call's own deadline, or a broker sync may reach its 15-minute deadline. The deadline bounds contact with the source, including an attempt in flight and every lane, budget, reset or backoff wait. It does not bound local work after both broker answers arrive. Nothing was written: a broker sync fetches operations and portfolio before its first write, and a market sync records no observation (its run is closed as partial) | wait what `Retry-After` says, then sync again. During a broker pause or closure, the tally refuses before a send; those states survive restart (§1.1). A repeat inside an in-process named wait or open breaker is likewise refused, but those are forgotten on restart. Otherwise `Retry-After` is the gateway's own backoff, and a repeat before it may be sent subject to its budgets |
| `{"code":"source_refused", …}` (502) | the source answered with a status the gateway does not retry. For a broker, each `4xx` except `429` — for example 401 or 403 for a revoked or wrong token — is sent once and counted in the tally; after the third within ten minutes, later calls are `source_unavailable` until the 30-minute closure ends. For a market source, any failure other than 429, 500, 502, 503 and 504 is sent once. No fact and no observation was written | broker: check the access, `iaam broker access` (§6.3); market source: check what the request names. Retrying an unchanged refusal repeats it and can close a broker host |
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
