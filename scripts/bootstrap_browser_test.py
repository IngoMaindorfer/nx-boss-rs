#!/usr/bin/env python3
"""Enroll a disposable passkey using a Chromium virtual authenticator."""
import subprocess
import sys
from pathlib import Path

subprocess.run(["node", str(Path(__file__).with_suffix(".js")), *sys.argv[1:]], check=True)
