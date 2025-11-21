"""Test SKIP phase return value cases."""

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_phase_result_skip(phase):
    """Call phase.SKIP → Skip outcome."""
    phase.skip("Skipping phase")
