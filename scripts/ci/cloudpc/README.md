# The cloud PC's CI runners

`.github/workflows/ci.yml` runs its Linux jobs on GitHub Actions runners
on the cloud PC: the WSL Ubuntu of the Windows PC (16 cores, 24 GB for
WSL). Each runner takes one job in a fresh container and is replaced when
the job ends; what should outlive a job lives on Docker volumes.

| File | What it is |
|---|---|
| `install.sh` | Sets the runners up, updates them, shows their status (`--status`) or removes them (`--remove`) |
| `Dockerfile` | The `imago-ci-runner` image: Ubuntu 24.04, the app's build and smoke packages, the pinned Rust toolchain, cargo-nextest, cargo-deny and GitHub's runner agent |
| `supervise.sh` | Keeps one runner slot serving jobs (the `imago-ci-runner@SLOT` units) |
| `entrypoint.sh` | Starts the runner agent in the container for one job |

## Slots

| Slot | Labels | Takes |
|---|---|---|
| `lint` | `cloudpc`, `lint` | Lint |
| `test` | `cloudpc`, `test` | Test |
| `smoke` | `cloudpc`, `smoke` | Smoke |
| `general-1`, `general-2` | `cloudpc`, `general` | Changes, Advisories, Site, Check |

Each slot has its own target directory (`imago-ci-target-SLOT`) and
checkout (`imago-ci-work-SLOT`). The checkout is kept so that files a
change leaves alone keep their modification times, and cargo rebuilds
only the crates the change touched. Keeping each role on its own slot
means each target directory holds one kind of build (clippy's, the tests'
or the app's) and is always warm for it. `CARGO_HOME` (`imago-ci-cargo`)
and the test ROMs are shared; cargo's lock lives in `CARGO_HOME`, so jobs
side by side wait for each other to unpack a crate rather than unpack it
twice. A target directory past 60 GB is cleared before the next
job.

## Isolation

The repository is public, so:

- `ci.yml` sends a pull request from a fork to GitHub's runners. Only
  pushes, manual runs and pull requests from branches of this repository
  run here.
- `install.sh` sets the repository to hold every outside contributor's
  workflow run until the owner approves it. A fork could otherwise edit
  `ci.yml` in its pull request to ask for these runners.
- A job's container mounts no host folder, runs as an unprivileged user
  with no capabilities, and gets only a one-job runner configuration,
  never the GitHub token. The `imago-ci` network's firewall lets it reach
  the internet, but not this PC, the LAN, the Windows host or the tailnet.
- The runners run in `imago-ci.slice`: idle CPU is theirs, and under
  contention interactive work and agents win about 4 to 1. CI gets at
  most 16 GB of memory.

Releases (`release-linux-windows.yml`) never build here: a package comes
from a clean machine, not from target directories pull requests wrote to.

## Day to day

```sh
scripts/ci/cloudpc/install.sh --status    # the units and GitHub's view of the runners
journalctl -u 'imago-ci-runner@*' -f      # what each slot is doing
```

Run `install.sh` again after the Rust toolchain (`rust-toolchain.toml`)
or the Linux build packages (`scripts/linux/install-build-deps.sh`)
change, so the image has them baked in. Until then a job installs a new
toolchain itself on every run.

If the cloud PC is down, set the repository variable `CLOUDPC_LINUX` to
`off` (Settings > Secrets and variables > Actions > Variables), and every
job runs on GitHub's runners; delete it, or set it to anything else, to
come back. To start over with cold caches:

```sh
scripts/ci/cloudpc/install.sh --remove
docker volume rm $(docker volume ls -q -f name=imago-ci)
scripts/ci/cloudpc/install.sh
```
