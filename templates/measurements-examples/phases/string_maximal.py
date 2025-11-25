import sys
import os
import random
def test_maximal_string(phase, run, ui):
    """Version string with regex validator, count aggregations, and pattern validation"""

    # Generate version strings for aggregation
    version_strings = []
    for _ in range(5):
        major = random.randint(1, 3)
        minor = random.randint(0, 9)
        patch = random.randint(0, 99)
        version_strings.append(f"v{major}.{minor}.{patch}")

    # Primary version string (will be validated against regex in YAML)
    primary_version = version_strings[0]

    # Count aggregations
    total_count = len(version_strings)
    unique_count = len(set(version_strings))

    measurements.version_string = primary_version

    run.log.info(f"Primary version: {primary_version}")
    run.log.info(f"Total versions processed: {total_count}")
    run.log.info(f"Unique versions: {unique_count}")
    run.log.info(f"All versions: {version_strings}")

    