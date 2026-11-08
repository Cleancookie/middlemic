TARGET := x86_64-pc-windows-gnu
EXE := target/$(TARGET)/release/middlemic.exe

.PHONY: build test open

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
