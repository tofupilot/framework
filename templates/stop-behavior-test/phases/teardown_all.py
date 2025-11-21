import time
import sys


def teardown_all(test_api, ui):
    """Cleanup procedure - shutdown test system (SHOULD RUN EVEN WHEN STOPPED)"""
    print("🛑 [CLEANUP_PROCEDURE] Shutting down test system...", file=sys.stderr)
    print("   - Saving session data", file=sys.stderr)
    print("   - Powering down equipment", file=sys.stderr)
    print("   - Cleaning up resources", file=sys.stderr)
    time.sleep(5)
    print("✅ [CLEANUP_PROCEDURE] Test system shutdown complete", file=sys.stderr)
