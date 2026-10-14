# Security

## Maintenance

gp64 is maintained irregularly as a personal project. No version has a
guaranteed security support period. Reports, responses, fixes and backports are
provided when the maintainer is able to work on them; none is guaranteed.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting for this repository:
[Report a vulnerability](https://github.com/0x4D44/gp64/security/advisories/new).
GitHub offers this feature for public repositories. It must be enabled in the
repository settings when publication occurs. If it is unavailable, wait for a
private reporting route before sharing exploit details; do not use a public issue
for sensitive reports.

Include the affected revision, operating system, expected and observed
behaviour, and a minimal reproduction using synthetic files where possible.
Do not send proprietary program files, captured code or data, credentials or
personal information. If a reproduction depends on owned media, describe the
dependency so a suitable private reproduction method can be agreed.

## Running gp64

Guest DOS file operations can read and modify the mounted host folder. Use
programs you trust and a separate copy of files you want to preserve. Host
settings and recordings also use locations described in the README.

Modem hosting opens a TCP listener and LAN discovery sends UDP announcements.
Use modem connections on networks and with peers you trust. These features
provide no authentication or encrypted transport.
