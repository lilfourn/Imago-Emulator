#!/bin/bash
# Keeps one runner slot on the cloud PC serving GitHub Actions jobs: asks
# GitHub for a just-in-time runner (it takes one job, then unregisters),
# starts it in a fresh imago-ci-runner container, waits for the job to
# end, and starts the next. Run by the imago-ci-runner@SLOT systemd unit
# that install.sh writes; SLOT names the role whose jobs it takes.
#
#   supervise.sh SLOT    SLOT is lint, test, smoke, general-1, ...
#
# The container is the only thing a job can touch: no host folder is
# mounted, it runs as an unprivileged user with no capabilities, and the
# imago-ci network's firewall rules (install.sh) let it reach the
# internet but not this PC, its LAN or the tailnet. GitHub's token stays
# here: the container gets only the one-job runner configuration.
#
# What a job should find warm lives on volumes: CARGO_HOME (the registry
# and the lock cargo takes to unpack into it) and the test ROMs, shared
# by every slot, and a target directory and checkout
# of each slot's own, so each role keeps rebuilding only what changed.
set -euo pipefail

slot="${1:?usage: supervise.sh SLOT}"
repo="${IMAGO_CI_REPO:-lilfourn/Imago-Emulator}"
image="${IMAGO_CI_IMAGE:-imago-ci-runner}"
# A target directory past this size is cleared before the next job:
# cargo never removes old artifacts itself.
max_target_gb="${IMAGO_CI_MAX_TARGET_GB:-60}"
role="${slot%-[0-9]*}"

log() { echo "[$slot] $*"; }

container="imago-ci-$slot"
docker rm -f "$container" >/dev/null 2>&1 || true
# Runners this slot left registered when it was last stopped.
gh api --paginate "repos/$repo/actions/runners" \
    --jq ".runners[] | select(.name | startswith(\"cloudpc-$slot-\")) | .id" 2>/dev/null |
    while read -r stale; do
        gh api -X DELETE "repos/$repo/actions/runners/$stale" >/dev/null 2>&1 || true
    done

while true; do
    target_kb=$(docker run --rm --entrypoint du -v "imago-ci-target-$slot:/t" "$image" -sk /t 2>/dev/null | cut -f1 || echo 0)
    if [ "${target_kb:-0}" -gt $((max_target_gb * 1024 * 1024)) ]; then
        log "target directory is $((target_kb / 1024 / 1024)) GB; clearing it"
        docker volume rm -f "imago-ci-target-$slot" >/dev/null
    fi

    name="cloudpc-$slot-$(date +%Y%m%d-%H%M%S)"
    if ! response=$(gh api -X POST "repos/$repo/actions/runners/generate-jitconfig" \
        -f name="$name" -F runner_group_id=1 -f work_folder=_work \
        -f 'labels[]=cloudpc' -f "labels[]=$role" 2>&1); then
        log "GitHub refused a runner configuration: $response"
        sleep 60
        continue
    fi
    id=$(jq -r .runner.id <<<"$response")
    log "runner $name ($id) waiting for a job"

    status=0
    jq -r .encoded_jit_config <<<"$response" | docker run --rm -i \
        --name "$container" \
        --hostname "$name" \
        --network imago-ci \
        --dns 1.1.1.1 --dns 8.8.8.8 \
        --cgroup-parent imago-ci.slice \
        --cap-drop ALL --security-opt no-new-privileges \
        --pids-limit 16384 --shm-size 2g \
        -v imago-ci-cargo:/cache/cargo \
        -v imago-ci-roms:/cache/test-roms \
        -v "imago-ci-target-$slot:/cache/target" \
        -v "imago-ci-work-$slot:/home/runner/actions-runner/_work" \
        "$image" || status=$?
    log "runner $name exited with $status"

    # A runner that never took its job stays listed until removed.
    gh api -X DELETE "repos/$repo/actions/runners/$id" >/dev/null 2>&1 || true
    [ "$status" = 0 ] || sleep 10
done
