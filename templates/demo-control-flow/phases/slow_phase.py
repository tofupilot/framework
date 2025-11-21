"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def slow_phase(phase, test_api, ui):
    """Sleeps for 5 seconds - will timeout with 2s timeout."""
    time.sleep(5)
    
