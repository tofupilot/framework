"""Phase that verifies the counter state persisted."""

import sys


def verify_count(counter):
    """Verify the counter maintained its state with direct plug injection."""
    current_count = counter.get_count()

    if current_count == 8:
        print(f"✅ Counter state persisted correctly! Count is {current_count}", file=sys.stderr)
    else:
        print(f"❌ Unexpected count: {current_count} (expected 8)", file=sys.stderr)
