# AOSC Buildit! Bot

Build automation with Telegram and GitHub Integrations.

## Setup Worker on a new Buildbot

Steps:

1. `mkdir -p /buildroots/buildit`
2. `cd /buildroots/buildit && git clone https://github.com/AOSC-Dev/buildit`
3. `cd /buildroots/buildit && ciel new`, making sure to create an instance named "main" when asked
4. `cp /buildroots/buildit/buildit/systemd/buildit-worker.service /etc/systemd/system`
5. `$EDITOR /etc/systemd/system/buildit-worker.service`：update ARCH and BUILDIT_AMQP_ADDR
6. `systemctl enable --now buildit-worker`
7. `chmod 600 /etc/systemd/system/buildit-worker.service`
