import time
import sys


def setup_each(run, ui):
    """Setup slot - prepare each unit for testing"""
    print("🔧 [SETUP_SLOT] Preparing unit for test...", file=sys.stderr)
    print("   - Connecting to unit", file=sys.stderr)
    print("   - Running diagnostics", file=sys.stderr)
    time.sleep(1)
    print("✅ [SETUP_SLOT] Unit preparation complete", file=sys.stderr)
