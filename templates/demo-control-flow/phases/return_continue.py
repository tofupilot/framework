"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_continue(phase, run, ui):
    """Returns CONTINUE, outcome is PASS/FAIL based on measurements."""
    time.sleep(0.25)
    
