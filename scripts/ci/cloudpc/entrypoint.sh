#!/bin/sh
# Starts GitHub's runner agent for one job, with the just-in-time
# configuration supervise.sh asked GitHub for and passed on stdin; the
# agent removes itself from the repository's runners when the job ends.
set -eu

IFS= read -r jitconfig
cd /home/runner/actions-runner
exec ./run.sh --jitconfig "$jitconfig" </dev/null
