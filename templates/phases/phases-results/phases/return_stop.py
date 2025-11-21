"""Test STOP phase return value cases."""

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_stop():
    """Return "Stop" → Stop (halts procedure execution)."""
    return "Stop"
