import time
import sys


def teardown_each(test_api, ui):
    """Cleanup slot - clean up after each unit (SHOULD RUN EVEN WHEN STOPPED)"""
    print("🧹 [CLEANUP_SLOT] Finalizing unit test...", file=sys.stderr)
    print("   - Saving test data", file=sys.stderr)
    print("   - Disconnecting from unit", file=sys.stderr)
    print("   - Resetting for next unit", file=sys.stderr)
    time.sleep(5)
    print("✅ [CLEANUP_SLOT] Unit test finalized", file=sys.stderr)
