# Deploying a Lode server on a Linux VDS: instructions for AI agents

This is a runbook for an AI agent (Claude Code or similar) that is asked to put a Lode server
on a Linux machine over SSH. Follow the steps in order. Every step says how to check that it
worked; do not go on to the next step while a check fails. The person-facing version of the
same material is chapter 2 of Part II in the user guide (`docs/user-guide.html`).

## 0. Rules

- **Ask, do not guess**, for: the SSH host and user, the domain name, the email for certificate
  notices, and where backups go. If the person has no domain, offer `<public-ip>.sslip.io`
  (step 1): HTTPS with a real certificate and nothing to register. Offer `--local` (plain
  HTTP on the machine itself) only for a test on that machine or behind their own proxy.
- **Never choose the admin password yourself.** Do not type one, put it in a command line,
  echo it, or write it to a file you created. Ask the person which way they want:
  - *They can use SSH:* they put the password on the server in a file only root can read,
    such as `/root/lode-admin-password.txt` (first line only), and tell you the path. Remove
    the file when setup is done (step 4), after asking.
  - *They cannot use SSH:* setup generates a first password (`--generate-admin-password`)
    and prints it once. Pass it to the person in your reply, once, and do not store it or
    repeat it later. It only lets them choose their own password at the first sign-in (web
    page or `lode login`), and it stops working after 24 hours if unused.
- `loded setup` prints no admin token. Do not ask for one (`--print-admin-token`) unless the
  person needs it for a script; then never print it again or store it.
- Changes to the machine (installing packages, opening ports, starting services) are what the
  person asked for. Deleting data is not: never remove `/var/lib/lode` or a backup directory.
- Use `sudo` when you are not root. Run commands one at a time and read the output.

## 1. Check the machine

```
ssh <user>@<host> 'cat /etc/os-release; uname -m; nproc; free -h; df -h /var'
```

- Supported: Debian, Ubuntu, Fedora, RHEL, Rocky, Alma, openSUSE, on `x86_64` or `aarch64`.
  Anything else: use the release archive (section 7), not the packages.
- 1 CPU and 1 GB of RAM are enough for a small team. Disk: depots take about the size of all
  file versions ever stored, compressed. Tell the person if `/var` has less than 20 GB free.
- systemd must be present (`systemctl --version`).

If the person gave a domain, check that it points at this machine:

```
getent hosts <domain>          # on the server; compare with: curl -s https://api.ipify.org
```

Stop and tell the person if the address differs. The certificate cannot be issued until DNS is
right.

**No domain:** use `<public-ip>.sslip.io` as the domain, for example `203.0.113.7.sslip.io`
(for IPv6, dashes instead of colons: `2001-db8--7.sslip.io`). sslip.io is a free public DNS
service that answers every such name with the address inside it, so Let's Encrypt works
unchanged. Check it the same way:

```
curl -s https://api.ipify.org                  # the public address, say 203.0.113.7
getent hosts 203.0.113.7.sslip.io              # must print that address
```

Tell the person that this depends on a third-party DNS service and suits a trial; for a team
that stays, a domain of their own is better (switching later means a new `acme_domain` in
`loded.toml` and a new server address for clients).

## 2. Open the port

Lode serves everything on one TCP port: **443** with a domain (Let's Encrypt uses the same port
for its check, the TLS-ALPN-01 challenge, so port 80 is not needed), or **7480** on
`127.0.0.1` with `--local`, which needs no opening.

- If `ufw` is active (`sudo ufw status`): `sudo ufw allow 443/tcp`.
- If `firewalld` is active (`sudo firewall-cmd --state`):
  `sudo firewall-cmd --permanent --add-service=https && sudo firewall-cmd --reload`.
- The VDS provider may also have a firewall in its control panel. You cannot see it from the
  machine; ask the person to check that TCP 443 is allowed in.

Check that nothing else holds the port: `sudo ss -ltnp | grep ':443 '` must print nothing. If a
web server (nginx, apache, caddy) is there, stop and ask the person: Lode can run behind it
instead (`--local`, then proxy to `127.0.0.1:7480`; see the user guide, "Behind a proxy").

## 3. Install the packages

```
curl -fsSL https://raw.githubusercontent.com/hutler/Lode/main/lode/packaging/install.sh | sh
```

This picks the `.deb` or `.rpm` of the newest `lode-v*` release for the machine, checks it
against `SHA256SUMS`, and installs `lode-server` (the server `loded`, the system user `lode`,
the systemd unit `loded.service`, data directory `/var/lib/lode`) and `lode` (the client).
For one version, add `sh -s -- --version 0.2.0` after the pipe.

Check:

```
loded --version
lode --version
id lode
systemctl cat loded | head -3
```

## 4. Set up the server

With a password file from the person: the `lode` user cannot read a file in `/root`, so the
password goes in on standard input (`--admin-password-file -`). It never appears in a command
line or in the output.

```
sudo cat /root/lode-admin-password.txt | sudo -u lode loded setup --yes \
    --domain <domain> --email <email> --admin-password-file -
```

Without SSH for the person: setup makes the first password and prints it once, under "Admin
password (shown only now …)".

```
sudo -u lode loded setup --yes --domain <domain> --email <email> --generate-admin-password
```

`<domain>` is the person's domain or the `sslip.io` name from step 1. Only for a test on
the machine itself, or behind the person's own TLS proxy, replace `--domain <domain> --email
<email>` with `--local` (plain HTTP on 127.0.0.1:7480).

Options worth knowing (all are in `loded setup --help`):

| Option | Default | Note |
| --- | --- | --- |
| `--data-dir` | `/var/lib/lode` | Keep the default: the systemd unit expects it |
| `--listen` | `0.0.0.0:443` with a domain, `127.0.0.1:7480` without | |
| `--backup-dir` | `/var/lib/lode/backups` | Better on another disk. A path outside `/var/lib/lode` needs step 4b |
| `--no-backup` | | Only when the person says backups are handled elsewhere |

Check: the output ends with "Lode is set up in /var/lib/lode" and the server address for
clients. `sudo test -f /var/lib/lode/loded.toml && echo ok` prints `ok`.

**4b. Backups outside the data directory.** The service may write only to `/var/lib/lode`.
For another backup directory:

```
sudo install -d -o lode -g lode <backup-dir>
sudo systemctl edit loded       # add the two lines below, save
[Service]
ReadWritePaths=<backup-dir>
```

Then, if there is one, ask the person whether to delete `/root/lode-admin-password.txt`, and
delete it if they agree.

## 5. Start it

```
sudo systemctl enable --now loded
sleep 5
systemctl is-active loded                  # active
journalctl -u loded -n 50 --no-pager       # no "error" lines
```

With a domain, the first start obtains the certificate; the log says so within a minute. If
the log shows a certificate error, check DNS (step 1) and the provider firewall (step 2), then
`sudo systemctl restart loded`. Let's Encrypt limits failed attempts: after three failures,
stop and tell the person rather than retrying.

## 6. Check from outside

From the agent's own machine or the person's PC (not the server):

```
curl -fsS https://<domain>/api/v1/info           # JSON with the server version
lode login https://<domain> --name admin         # the person types the password
lode whoami
```

With a generated first password, the person's first sign-in (the admin web page or `lode
login`) asks them to choose their own password; until then the generated one works for
nothing else. If it ran out (24 hours), make a new one on the server with
`sudo -u lode loded reset-admin --generate-password --config /var/lib/lode/loded.toml`.

With `--local`, run the `curl` on the server against `http://127.0.0.1:7480/api/v1/info`.

The admin web page is `https://<domain>/admin/` (sign in as `admin`). Give the person:

- the server address,
- the admin page address,
- what to do next (section 8),
- where the logs and backups are: `journalctl -u loded`, and the backup directory.

## 7. Without the packages (other distributions)

Download `lode-<version>-<arch>-unknown-linux-musl.tar.gz` from
<https://github.com/hutler/Lode/releases>, check it against `SHA256SUMS`, unpack it, and put
`loded` and `lode` in `/usr/local/bin`. Then:

```
sudo useradd --system --home-dir /var/lib/lode --shell /usr/sbin/nologin lode
sudo install -d -o lode -g lode /var/lib/lode
sudo -u lode loded setup --yes --domain <domain> --email <email> --admin-password-file -
sudo loded service install --config /var/lib/lode/loded.toml
sudo systemctl enable --now loded
```

Then continue with steps 5 and 6. A container is also possible: `packaging/compose.yaml`
with the image `ghcr.io/hutler/loded:<version>` (see the user guide, "Container").

## 8. After the server runs

These are for the person, or for you if they ask:

```
lode login https://<domain>                                  # as admin
lode admin depot create game
lode admin user create alice                                 # prompts for her password
lode admin acl add allow write user:alice --depot game
cd <project folder> && lode init https://<domain>/game --template unreal -m "Import"
```

Templates: `unreal`, `unity`, `godot`, `rust`, `node`, `generic`. A Git project keeps its
history with `lode import git <repo> https://<domain>/game --branch main`.

## 9. Upgrades and troubleshooting

- **Upgrade:** run the install command from step 3 again, then
  `sudo systemctl restart loded`. Read `docs/UPGRADING.md` first for the versions in between.
- **Status and logs:** `systemctl status loded`, `journalctl -u loded -f`.
- **Configuration:** `/var/lib/lode/loded.toml`. Restart after editing. Every key is in the
  user guide, Part II, chapter 3.
- **Port 443 refused from outside but fine on the server:** the provider firewall (step 2).
- **"permission denied" writing backups:** step 4b.
- **Forgotten admin password:** `sudo -u lode loded reset-admin --generate-password --config
  /var/lib/lode/loded.toml` (with the server stopped) prints a new first password; give it to
  the person once, as in step 4.
