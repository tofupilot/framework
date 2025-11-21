"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def never_executed_phase(phase, test_api, ui):
    """Should never execute because previous phase returned STOP."""
    print("ERROR: This phase should never execute!", file=sys.stderr)
    time.sleep(0.25)
    
