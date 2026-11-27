#!/bin/bash
# Sets up (or updates) the cloud PC's GitHub Actions runners for
# .github/workflows/ci.yml, on the WSL Ubuntu of the Windows PC. Run it
# from the repository, as yourself, with `gh` signed in as the
# repository's owner; it asks sudo for the system parts:
#
#   scripts/ci/cloudpc/install.sh            install, or update after a change here
#   scripts/ci/cloudpc/install.sh --status   what the runners are doing
#   scripts/ci/cloudpc/install.sh --remove   stop and remove everything but the volumes
#
# It installs Docker Engine if it is missing, builds the imago-ci-runner
# image (Dockerfile), creates the imago-ci network and its firewall, and
# starts one imago-ci-runner@SLOT unit per slot (supervise.sh). Run it again
# after the Rust toolchain or the Linux build packages change, so the
# image has them baked in.
#
# The slots: one each for the lint, test and smoke jobs, so each keeps the
# warmest target directory for its own job, and two general ones for the
# short jobs. They run in imago-ci.slice, which yields the CPU to
# everything else on this PC when both want it, and caps CI's memory so
# agents and the T3 server keep theirs.
#
# This repository is public, so ci.yml sends a pull request from a fork
# to GitHub's runners, never here, and this script makes GitHub hold
# every outside contributor's workflow run until the owner approves it:
# a fork could otherwise edit ci.yml itself to ask for these runners.
set -euo pipefail

repo="${IMAGO_CI_REPO:-lilfourn/Imago-Emulator}"
slots=(lint test smoke general-1 general-2)
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
lib=/usr/local/lib/imago-ci

units() { systemctl list-units --all --plain --no-legend 'imago-ci-runner@*' | awk '{print $1}'; }

case "${1:-}" in
    --status)
        systemctl --no-pager status imago-ci.slice 'imago-ci-runner@*' 2>/dev/null | grep -E '●|Active:|Memory:|CPU:' || true
        echo
        gh api "repos/$repo/actions/runners" --jq '.runners[] | "\(.name)  \(.status)  busy=\(.busy)"'
        exit 0
        ;;
    --remove)
        mapfile -t running < <(units)
        [ "${#running[@]}" = 0 ] || sudo systemctl disable --now "${running[@]}"
        sudo rm -f /etc/systemd/system/imago-ci-runner@.service /etc/systemd/system/imago-ci.slice \
            /etc/systemd/system/imago-ci-firewall.service
        sudo rm -rf "$lib"
        sudo systemctl daemon-reload
        docker network rm imago-ci >/dev/null 2>&1 || true
        echo "Removed. The cache volumes remain: docker volume ls -q -f name=imago-ci"
        exit 0
        ;;
    "") ;;
    *)
        sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
        exit 2
        ;;
esac

gh auth status >/dev/null

echo "== Docker Engine"
if ! command -v dockerd >/dev/null; then
    sudo DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=600 install -y -qq docker.io docker-buildx
fi
sudo systemctl enable --now docker >/dev/null
id -nG | grep -qw docker || sudo usermod -aG docker "$USER"

echo "== The runner image"
docker build -q -f "$here/Dockerfile" -t imago-ci-runner "$root"

echo "== The imago-ci network and its firewall"
docker network inspect imago-ci >/dev/null 2>&1 ||
    docker network create -o com.docker.network.bridge.name=br-imago-ci imago-ci >/dev/null
sudo install -d "$lib"
sudo install -m 755 "$here/supervise.sh" "$lib/supervise.sh"
sudo tee "$lib/firewall.sh" >/dev/null <<'EOF'
#!/bin/sh
# A job's container may reach the internet, and nothing private: not this
# PC (its T3 server, SSH), the LAN, WSL's Windows host, or the tailnet.
set -eu
iptables -N IMAGO-CI 2>/dev/null || iptables -F IMAGO-CI
iptables -A IMAGO-CI -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
for net in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 100.64.0.0/10 169.254.0.0/16; do
    iptables -A IMAGO-CI -d "$net" -j REJECT
done
iptables -C DOCKER-USER -i br-imago-ci -j IMAGO-CI 2>/dev/null ||
    iptables -I DOCKER-USER -i br-imago-ci -j IMAGO-CI
iptables -C INPUT -i br-imago-ci -j REJECT 2>/dev/null ||
    iptables -I INPUT -i br-imago-ci -j REJECT
EOF
sudo chmod 755 "$lib/firewall.sh"

echo "== systemd units"
sudo tee /etc/systemd/system/imago-ci.slice >/dev/null <<'EOF'
[Unit]
Description=Imago Emulator CI runners

[Slice]
# Idle CPU is CI's; under contention, interactive work wins about 4:1.
CPUWeight=25
IOWeight=25
# WSL has 24 GB. CI gets most of it, reclaimed first under pressure.
MemoryHigh=14G
MemoryMax=16G
MemorySwapMax=4G
EOF
sudo tee /etc/systemd/system/imago-ci-firewall.service >/dev/null <<EOF
[Unit]
Description=Firewall for the Imago Emulator CI network
After=docker.service
Requires=docker.service
PartOf=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=$lib/firewall.sh

[Install]
WantedBy=docker.service
EOF
sudo tee /etc/systemd/system/imago-ci-runner@.service >/dev/null <<EOF
[Unit]
Description=Imago Emulator CI runner %i
After=docker.service imago-ci-firewall.service network-online.target
Requires=docker.service imago-ci-firewall.service
Wants=network-online.target

[Service]
User=$USER
Environment=HOME=$HOME IMAGO_CI_REPO=$repo
Environment=PATH=$(dirname "$(command -v gh)"):/usr/local/bin:/usr/bin:/bin
ExecStart=$lib/supervise.sh %i
Restart=always
RestartSec=15
KillMode=mixed
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now imago-ci-firewall.service >/dev/null
sudo systemctl restart imago-ci-firewall.service

echo "== Runners"
mapfile -t running < <(units)
for unit in "${running[@]}"; do
    slot="${unit#imago-ci-runner@}"
    slot="${slot%.service}"
    printf '%s\n' "${slots[@]}" | grep -qx "$slot" || sudo systemctl disable --now "$unit"
done
busy=$(gh api "repos/$repo/actions/runners" --jq '.runners[] | select(.busy) | .name')
for slot in "${slots[@]}"; do
    unit="imago-ci-runner@$slot.service"
    sudo systemctl enable "$unit" >/dev/null 2>&1
    # A slot in the middle of a job finishes it on the old image and unit;
    # an idle one starts again on the new ones.
    if grep -q "^cloudpc-$slot-" <<<"$busy"; then
        echo "  $slot is running a job; it updates after the next job if you run this again"
        sudo systemctl start "$unit"
    else
        sudo systemctl restart "$unit"
    fi
done

echo "== Outside contributors' workflow runs wait for approval"
gh api -X PUT "repos/$repo/actions/permissions/fork-pr-contributor-approval" \
    -f approval_policy=all_external_contributors >/dev/null

echo
echo "Done. ${#slots[@]} runners serve $repo:"
sleep 5
gh api "repos/$repo/actions/runners" --jq '.runners[] | "  \(.name)  \(.status)"'
