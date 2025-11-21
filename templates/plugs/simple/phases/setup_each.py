"""Setup phase that triggers each-slot plug initialization."""

import sys


def setup_each():
    """Initialize slot - each-slot plugs are created automatically."""
    print("🔧 Setting up slot - Counter plug will be initialized", file=sys.stderr)
