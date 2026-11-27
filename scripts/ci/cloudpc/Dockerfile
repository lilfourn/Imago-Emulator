# The cloud PC's CI runner: Ubuntu 24.04 with everything a Linux job in
# .github/workflows/ci.yml needs already installed, so a job starts with
# its first real step. The app's build and smoke packages, the Rust
# toolchain rust-toolchain.toml names, cargo-nextest, cargo-deny, and
# GitHub's runner agent, each download checked against its SHA-256.
#
# Built by scripts/ci/cloudpc/install.sh from the repository root:
#
#   docker build -f scripts/ci/cloudpc/Dockerfile -t imago-ci-runner .
#
# Each container runs one job and exits (supervise.sh starts the next).
# What should outlive a job lives on volumes: the cargo registry, a target
# directory and the checkout for each runner slot, and the test ROMs.
FROM ubuntu:24.04

ARG RUNNER_VERSION=2.338.2
ARG RUNNER_SHA256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40
ARG NEXTEST_VERSION=0.9.151
ARG NEXTEST_SHA256=1d4ce302b820c41f4b89c5fb9bce45f0f0c94882c1cf08fc360caf08be6e7df4
ARG DENY_VERSION=0.20.2
ARG DENY_SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f

# The app's build and smoke-test packages, as every other Linux machine
# gets them, plus what the runner and the jobs use themselves.
COPY scripts/linux/install-build-deps.sh /tmp/install-build-deps.sh
RUN sh /tmp/install-build-deps.sh --smoke \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      git jq xz-utils zstd \
 && rm -rf /var/lib/apt/lists/* /tmp/install-build-deps.sh

# GitHub's runner agent and the libraries it needs (ICU, Kerberos, LTTng).
RUN useradd --create-home --uid 1001 --shell /bin/bash runner \
 && mkdir -p /home/runner/actions-runner /cache \
 && cd /home/runner/actions-runner \
 && curl -fsSL -o runner.tar.gz \
      "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz" \
 && echo "${RUNNER_SHA256}  runner.tar.gz" | sha256sum -c - \
 && tar -xzf runner.tar.gz && rm runner.tar.gz \
 && ./bin/installdependencies.sh >/dev/null \
 && rm -rf /var/lib/apt/lists/* \
 && chown -R runner:runner /home/runner /cache

# cargo-nextest runs every test binary at once, where cargo test runs them
# one after another; cargo-deny checks Cargo.lock against RustSec.
RUN curl -fsSL -o /tmp/nextest.tar.gz \
      "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
 && echo "${NEXTEST_SHA256}  /tmp/nextest.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/nextest.tar.gz -C /usr/local/bin cargo-nextest \
 && curl -fsSL -o /tmp/deny.tar.gz \
      "https://github.com/EmbarkStudios/cargo-deny/releases/download/${DENY_VERSION}/cargo-deny-${DENY_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
 && echo "${DENY_SHA256}  /tmp/deny.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/deny.tar.gz -C /usr/local/bin --strip-components=1 "cargo-deny-${DENY_VERSION}-x86_64-unknown-linux-musl/cargo-deny" \
 && rm /tmp/nextest.tar.gz /tmp/deny.tar.gz

USER runner
WORKDIR /home/runner

# The toolchain rust-toolchain.toml pins, installed once here rather than
# by each job. A bump installs the new one on first use; rebuilding the
# image (install.sh) bakes it in again.
ENV RUSTUP_HOME=/home/runner/.rustup \
    CARGO_HOME=/home/runner/.cargo \
    PATH=/home/runner/.cargo/bin:/home/runner/actions-runner/externals/node24/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
# rustup 1.28.2 by its published SHA-256, as release-linux-windows.yml.
ARG RUSTUP_SHA256=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c
COPY --chown=runner:runner rust-toolchain.toml /tmp/toolchain/rust-toolchain.toml
RUN curl -fsSL -o /tmp/rustup-init \
      https://static.rust-lang.org/rustup/archive/1.28.2/x86_64-unknown-linux-gnu/rustup-init \
 && echo "${RUSTUP_SHA256}  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init \
 && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none \
 && rm /tmp/rustup-init \
 && cd /tmp/toolchain && rustup toolchain install \
 && rustup default "$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)" \
 && rm -rf /tmp/toolchain \
 && mkdir -p /home/runner/.cargo/registry /home/runner/.cargo/git \
      /home/runner/actions-runner/_work /cache/target /cache/test-roms

# Volumes supervise.sh mounts: the registry is shared, the rest per slot.
#   /home/runner/.cargo/registry, /home/runner/.cargo/git  cargo's downloads
#   /cache/target   CARGO_TARGET_DIR
#   /home/runner/actions-runner/_work  the checkout, kept so that files a
#                   change leaves alone keep their times and cargo doesn't
#                   rebuild the crates they belong to
#   /cache/test-roms  scripts/fetch-test-roms.sh's downloads
ENV CARGO_TARGET_DIR=/cache/target \
    CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=1 \
    IMAGO_CI_CACHE=/cache \
    RUNNER_ALLOW_RUNASROOT=0

COPY --chown=runner:runner scripts/ci/cloudpc/entrypoint.sh /home/runner/entrypoint.sh
ENTRYPOINT ["/home/runner/entrypoint.sh"]
