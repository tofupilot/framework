import time
import sys


def setup_all(run, ui):
    """Setup procedure - initialize the test system"""
    print("🚀 [SETUP_PROCEDURE] Initializing test system...", file=sys.stderr)
    print("   - Starting test equipment", file=sys.stderr)
    print("   - Loading configuration", file=sys.stderr)
    time.sleep(1)
    print("✅ [SETUP_PROCEDURE] Test system initialized", file=sys.stderr)
