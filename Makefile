TARGET := x86_64-pc-windows-gnu
EXE := target/$(TARGET)/release/middlemic.exe

.PHONY: build test open

build: ## Cross-compile the Windows exe (a no-op when nothing changed)
	cargo build --release --target $(TARGET)

test: ## DSP and UI tests, run natively
	cargo test --release

# Windows asks "Run?" for an exe on \\wsl.localhost, so open runs a copy on the local disk.
LOCAL_BIN = $(shell wslpath "$$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")/middlemic/bin

open: build ## Build if needed, copy to %LOCALAPPDATA%\middlemic\bin, then start it on Windows (needs WSL)
	@command -v wslpath >/dev/null || { echo "make open needs WSL to start a Windows program"; exit 1; }
	@mkdir -p "$(LOCAL_BIN)"
	@cp $(EXE) "$(LOCAL_BIN)/" || { echo "copy failed: close the running middlemic first"; exit 1; }
	@setsid nohup "$(LOCAL_BIN)/middlemic.exe" >/dev/null 2>&1 &
	@echo "started $(LOCAL_BIN)/middlemic.exe"
