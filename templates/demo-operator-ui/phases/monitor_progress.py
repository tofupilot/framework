"""Operator UI phase examples"""

import time
import base64
import urllib.request


def monitor_progress(log, phase, ui):
    """Progress bar and output components with auto-update"""
    log.info("Starting progress monitoring")

    stages = [
        ("Initializing...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage1"),
        ("Loading data...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage2"),
        ("Processing...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage3"),
        ("Validating...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage4"),
        ("Finalizing...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage5"),
        ("Complete!", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Success"),
    ]

    for i in range(0, 101, 20):
        stage_index = min(i // 20, len(stages) - 1)
        status_text, qr_url = stages[stage_index]

        log.info(f"Progress: {i}% - {status_text}")
        ui.progress = str(i)
        ui.Status = status_text

        try:
            with urllib.request.urlopen(qr_url) as response:
                image_data = response.read()
                image_base64 = base64.b64encode(image_data).decode('utf-8')
                ui.image = f'data:image/png;base64,{image_base64}'
        except Exception as e:
            log.error(f"Failed to load QR code: {e}")
            ui.image = ''

        time.sleep(0.8)

    log.info("Progress complete")
