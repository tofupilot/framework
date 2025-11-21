"""Phase that uses the SimpleCounter plug."""


def count_steps(counter):
    """Run the counting phase with direct plug injection."""
    counter.increment(5)
    counter.increment(3)
    counter.get_count()
