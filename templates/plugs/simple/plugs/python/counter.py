"""Minimal slot-level plug example - a simple counter."""


class SimpleCounter:
    """A simple counter plug that demonstrates slot-level lifecycle."""

    def __init__(self):
        print(f"🎬 SimpleCounter: Initializing...")
        self.count = 0
        print(f"✅ SimpleCounter: Ready! Starting at {self.count}")

    def __del__(self):
        print(f"🛑 SimpleCounter: Cleaning up... Final count: {self.count}")

    def increment(self, amount=1):
        """Increment the counter."""
        self.count += amount
        print(f"📊 Counter incremented by {amount}. New value: {self.count}")
        return self.count

    def get_count(self):
        """Get current count."""
        print(f"📊 Current count: {self.count}")
        return self.count

    def reset(self):
        """Reset counter to zero."""
        self.count = 0
        print(f"🔄 Counter reset to {self.count}")
        return self.count
