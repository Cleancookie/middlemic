TARGET := x86_64-pc-windows-gnu
EXE := target/$(TARGET)/release/middlemic.exe

.PHONY: build test open

build: ## Cross-compile the Windows exe (a no-op when nothing changed)
	cargo build --release --target $(TARGET)

test: ## DSP and UI tests, run natively
	cargo test --release

open: build ## Build if needed, then start the UI on Windows (needs WSL)
	@command -v wslpath >/dev/null || { echo "make open needs WSL to start a Windows program"; exit 1; }
	@setsid nohup ./$(EXE) >/dev/null 2>&1 &
	@echo "started $(EXE)"
