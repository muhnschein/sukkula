# Sukkula's build and check entry points.
#
# The rule, from vuo: `make check` runs exactly what CI's per-pull-request
# gate runs (.github/workflows/ci.yml), job for job, from a clean checkout,
# with no phone and no Sailfish SDK. Three jobs are left out because they
# need the network rather than this tree -- `deny` (the RustSec advisory
# database), `vendor` (open-quickshare at the pinned commit) and
# `wormhole-interop` (the Python client from PyPI) -- and are targets of
# their own. The device RPM is `make rpm`, which needs Docker
# and the SDK image; its Harbour validation is part of it.
#
# A missing tool fails, like CI's strict modes: a green `make check` that
# skipped the QML, the cross build or the fuzzers is a green that means
# nothing. The first run fetches the pinned toolchains (rustup), which is
# network; nothing after that is.
#
# Host packages (Debian/Ubuntu):
#   dbus libdbus-1-dev pkg-config            the bluetooth feature and its tests
#   gcc-aarch64-linux-gnu g++-aarch64-linux-gnu binutils-aarch64-linux-gnu qemu-user
#   qtbase5-dev qtdeclarative5-dev qtdeclarative5-dev-tools g++ make
#   qml-module-qtquick2 qml-module-qttest
#   qml-module-qtquick-window2 qml-module-qtquick-layouts qttools5-dev-tools
#   rpm file binutils desktop-file-utils shellcheck
# and: cargo install --locked cargo-fuzz cargo-deny; pip install actionlint-py

CARGO ?= cargo
# The nightly the fuzzers use; ci.yml pins the same.
SUKKULA_NIGHTLY ?= nightly-2026-09-15
FUZZ_SECONDS ?= 60
# The toolchain fuzz-smoke fuzzes with; the pinned nightly unless set.
FUZZ_TOOLCHAIN ?= $(SUKKULA_NIGHTLY)
FEATURES := localsend quickshare wormhole bluetooth none
export SUKKULA_NIGHTLY

.PHONY: all check fmt fmt-check lint test rqs-lib-tests doc features deny deps \
        lockfile fuzz-lint fuzz-smoke ffi-asan cross qml cpp packaging vendor harbour \
        wormhole-interop sonar-reports rpm sdk-image clean help

all: check

## check: every per-pull-request CI job that needs no network: test,
## features, deps, fuzz-smoke, ffi-asan, cross, qml, cpp, packaging, harbour,
## and the vendor check's selftest. Not deny, vendor or wormhole-interop
## (network), not rpm (SDK).
check: fmt-check lint test rqs-lib-tests doc features deps lockfile harbour packaging \
       qml cpp cross ffi-asan fuzz-smoke
	./ci/vendor-check-selftest.sh
	@echo "== make check passed (deny, vendor and wormhole-interop need the network) =="

## fmt: format the workspace
fmt:
	$(CARGO) fmt --all

fmt-check:
	@echo "== rustfmt =="
	$(CARGO) fmt --all --check

## lint: clippy over the workspace, tests included, warnings denied; then
## the proof that clippy.toml's bans fire (ci/clippy-bans-selftest.sh)
lint:
	@echo "== clippy =="
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	./ci/clippy-bans-selftest.sh

test:
	@echo "== tests =="
	$(CARGO) test --workspace --locked

## rqs-lib-tests: the vendored Quick Share library's and mdns-sd's own tests,
## from copies; third_party/ is left untouched. mdns-sd's test-only crates
## come from cargo's cache or crates.io (third_party/mdns-sd.patches/test.sh)
rqs-lib-tests:
	@echo "== rqs_lib's own tests =="
	./ci/rqs-lib-tests.sh
	@echo "== the vendored mdns-sd's own tests =="
	./third_party/mdns-sd.patches/test.sh

## doc: rustdoc, with broken intra-doc links as errors
doc:
	@echo "== rustdoc =="
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --locked

## features: each protocol alone, and none (spec §3)
features:
	@for f in $(FEATURES); do \
		if [ "$$f" = none ]; then args="--no-default-features"; \
		else args="--no-default-features --features $$f"; fi; \
		echo "== sukkula-engine $$args =="; \
		$(CARGO) clippy -p sukkula-engine $$args --all-targets --locked -- -D warnings || exit 1; \
		$(CARGO) test -p sukkula-engine $$args --locked || exit 1; \
	done

## deny: licences, advisories, duplicates, bans, sources (deny.toml). Network.
deny:
	@command -v cargo-deny >/dev/null 2>&1 || { \
		echo "cargo-deny is not installed: cargo install --locked cargo-deny" >&2; exit 1; }
	$(CARGO) deny --locked check

## deps: the dependency budget (ci/check-deps.sh), proved by its selftest
deps:
	./ci/check-deps-selftest.sh
	./ci/check-deps.sh

## lockfile: committed, current, every git dependency pinned by rev
lockfile:
	./ci/check-lockfile.sh

## fuzz-lint: every target's seeds and dictionary (ci/check-dicts.sh, and its
## self-test) and the smoke's lengths and boundary inputs (its self-test);
## clippy over the fuzz crate (its own workspace), on the pinned toolchain, so
## a target that stopped compiling fails before any fuzzing; then the
## harness's tests (the generated seeds still decode and reach)
fuzz-lint:
	./ci/check-dicts.sh --self-test
	./ci/check-dicts.sh
	./scripts/fuzz-smoke.sh --self-test
	@echo "== clippy, fuzz/ =="
	$(CARGO) clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings
	$(CARGO) test --manifest-path fuzz/Cargo.toml --lib --locked

## fuzz-smoke: every cargo-fuzz target for FUZZ_SECONDS, from its seeds
fuzz-smoke: fuzz-lint
	FUZZ_TOOLCHAIN=$(FUZZ_TOOLCHAIN) ./scripts/fuzz-smoke.sh $(FUZZ_SECONDS)

## ffi-asan: the C harness over the C ABI, under AddressSanitizer
ffi-asan:
	./ci/ffi-harness/run.sh

## cross: the aarch64 engine. With SYSROOT=<SDK target sysroot> (and the SDK's
## /opt/cross), the release route; without, the Ubuntu-GCC smoke CI runs.
## Either way, then run under qemu-aarch64 against written bionic TLS slots.
cross:
	./ci/check-elf-selftest.sh
ifdef SYSROOT
	./scripts/cross-build-rust.sh --sdk "$(SYSROOT)"
else
	./scripts/cross-build-rust.sh --host
endif
	./ci/hybris-tls-test.sh

## qml: qmllint over qml/, and the UI's QML tests offscreen
qml:
	./ci/qml-lint.sh
	QT_QPA_PLATFORM=offscreen ./tests/run-qml-tests.sh

## cpp: the C++ shell's host tests (tests/README.md), against the stub
## engine and then the real one
cpp:
	@echo "== C++ tests, stub engine =="
	./tests/run-cpp-tests.sh
	@echo "== C++ tests, real engine =="
	$(CARGO) build -p sukkula-ffi --locked
	@t=$${CARGO_TARGET_DIR:-target}; case $$t in /*) ;; *) t="$(CURDIR)/$$t" ;; esac; \
		echo "SUKKULA_ENGINE=rust SUKKULA_RUST_LIB=$$t/debug/libsukkula_ffi.a ./tests/run-cpp-tests.sh"; \
		SUKKULA_ENGINE=rust SUKKULA_RUST_LIB="$$t/debug/libsukkula_ffi.a" ./tests/run-cpp-tests.sh

## packaging: spec, desktop entry, catalogs, shellcheck, actionlint; then
## the proofs for ci/apt-install.sh and scripts/sonar-report.sh
packaging:
	PACKAGING_LINT_STRICT=1 ./ci/packaging-lint.sh
	./ci/apt-install-selftest.sh
	./ci/sonar-report-selftest.sh

## vendor: third_party/rqs_lib is upstream plus its patches. Network.
vendor:
	./ci/vendor-check-selftest.sh
	./ci/vendor-check.sh

## wormhole-interop: Sukkula against the Python magic-wormhole client, pinned
## by hash (ci/wormhole-interop-requirements.txt), both ways. Network (PyPI),
## and Python 3.12 (PYTHON=...).
PYTHON ?= python3.12
wormhole-interop:
	@t=$${CARGO_TARGET_DIR:-target}; case $$t in /*) ;; *) t="$(CURDIR)/$$t" ;; esac; \
		v="$$t/wormhole-interop-venv"; rm -rf "$$v"; \
		$(PYTHON) -m venv "$$v" && \
		"$$v/bin/pip" install --quiet --require-hashes --only-binary=:all: \
			-r ci/wormhole-interop-requirements.txt && \
		SUKKULA_PY_WORMHOLE="$$v/bin/wormhole" $(CARGO) test -p sukkula-engine \
			--no-default-features --features wormhole --locked \
			--test wormhole_interop -- --include-ignored

## sonar-reports: the coverage report SonarQube Cloud imports
## (.github/workflows/build.yml), written to target/sonar/lcov.info: the
## workspace's tests under `cargo llvm-cov`, minus third_party/ and vendor/,
## which are upstream's code and not ours to cover. The scanner only imports
## coverage, so without this the reading is 0.0%. A report, not part of
## `check`; it needs:
##   rustup component add llvm-tools-preview
##   cargo install --locked cargo-llvm-cov
sonar-reports:
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { \
		echo "cargo-llvm-cov is not installed:" >&2; \
		echo "    rustup component add llvm-tools-preview" >&2; \
		echo "    cargo install --locked cargo-llvm-cov" >&2; \
		exit 1; }
	@mkdir -p target/sonar
	$(CARGO) llvm-cov --workspace --locked \
		--ignore-filename-regex '(^|/)(third_party|vendor)/' \
		--lcov --output-path target/sonar/lcov.info
	@echo "== wrote target/sonar/lcov.info =="

## harbour: the source-level Harbour gate, then the proof that it bites
harbour:
	HARBOUR_CHECK_STRICT=1 ./ci/harbour-check.sh
	./ci/harbour-check-selftest.sh

## rpm: the device RPM, as rpm.yml builds it, then Jolla's validator on it.
## Needs Docker and sudo (docs/BUILDING.md).
rpm:
	./scripts/build-rpm.sh all

## sdk-image: derive the SDK image rpm builds with (docs/BUILDING.md)
sdk-image:
	./ci/build-sdk-image.sh 5.2.0.15 aarch64 "$$(./scripts/build-rpm.sh image)"

clean:
	$(CARGO) clean
	rm -rf build-sfos RPMS .mb2

help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'
