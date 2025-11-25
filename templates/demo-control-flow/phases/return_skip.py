"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_skip(phase, run, ui):
    """Returns SKIP, outcome is SKIP, measurements ignored."""
    time.sleep(0.25)
    phase.skip("Skipping phase")
