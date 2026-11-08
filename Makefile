TARGET := x86_64-pc-windows-gnu
EXE := target/$(TARGET)/release/middlemic.exe

.PHONY: build test open release
SHELL := bash

# `open` is for trying changes: the quick profile skips LTO, so a rebuild takes about a second instead of 20.
QUICK_EXE := target/$(TARGET)/quick/middlemic.exe

build: ## Cross-compile the Windows exe (a no-op when nothing changed)
	cargo build --release --target $(TARGET)

test: ## DSP and UI tests, run natively
	cargo test --release

# Windows asks "Run?" for an exe on \\wsl.localhost, so open runs a copy on the local disk.
LOCAL_BIN = $(shell wslpath "$$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")/middlemic/bin

open: ## Quick-build, copy to %LOCALAPPDATA%\middlemic\bin, then start it on Windows (needs WSL)
	@command -v wslpath >/dev/null || { echo "make open needs WSL to start a Windows program"; exit 1; }
	@cargo build --profile quick --target $(TARGET)
	@mkdir -p "$(LOCAL_BIN)"
	@cp $(QUICK_EXE) "$(LOCAL_BIN)/" || { echo "copy failed: close the running middlemic first"; exit 1; }
	@setsid nohup "$(LOCAL_BIN)/middlemic.exe" >/dev/null 2>&1 &
	@echo "started $(LOCAL_BIN)/middlemic.exe"

# Tags the next version (from the commit emoji, or V=vX.Y.Z to choose), then pushes main and
# the tag. .github/workflows/release.yml builds and publishes it; running apps update from it.
release: test
	@test -z "$$(git status --porcelain --untracked-files=no)" || { echo 'commit or stash first'; exit 1; }
	@test "$$(git branch --show-current)" = main || { echo 'release from main'; exit 1; }
	git fetch -q origin main
	@git merge-base --is-ancestor origin/main HEAD || { echo 'origin/main has commits main lacks: pull first'; exit 1; }
	$(eval V ?= $(shell scripts/next-version.sh))
	@[[ "$(V)" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$$ ]] || { echo 'V must look like v1.2.3'; exit 1; }
	git tag -a $(V) -m "middlemic $(V)"
	git push --atomic origin main $(V)
	@echo "released $(V): https://github.com/Cleancookie/middlemic/actions"
