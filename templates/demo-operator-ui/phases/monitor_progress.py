"""Operator UI phase examples"""

import time


def monitor_progress(log, phase, ui):
    """Progress bar and output components with auto-update"""
    log.info("Starting progress monitoring")

    stages = [
        (
            "Initializing...",
            "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage1",
        ),
        (
            "Loading data...",
            "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage2",
        ),
        (
            "Processing...",
            "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage3",
        ),
        (
            "Validating...",
            "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage4",
        ),
        (
            "Finalizing...",
            "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage5",
        ),
        (
            "Complete!",
            "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Success",
        ),
    ]

    for i in range(0, 101, 20):
        stage_index = min(i // 20, len(stages) - 1)
        status_text, qr_url = stages[stage_index]

        log.info(f"Progress: {i}% - {status_text}")
        ui.progress = str(i)
        ui.text = status_text
        ui.image = qr_url
        time.sleep(0.8)

    log.info("Progress complete")
    
