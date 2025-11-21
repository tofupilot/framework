"""Test FAIL phase return value cases."""

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_phase_result_fail(phase):
    """Call phase.fail() → Fail outcome."""
    phase.fail("Phase intentionally failed for testing")
