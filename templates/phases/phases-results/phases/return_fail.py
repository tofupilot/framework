"""Test FAIL phase return value cases."""

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_fail():
    """Return "Fail" → Fail outcome."""
    return "Fail"
