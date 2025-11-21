import base64
import io
from PIL import Image, ImageDraw

def run(test, ui):
    try:
        import qrcode
        qr = qrcode.QRCode(version=1, box_size=10, border=4)
        qr.add_data("TofuPilot Image Demo - QR Code Test")
        qr.make(fit=True)

        qr_img = qr.make_image(fill_color="black", back_color="white")

        qr_buffer = io.BytesIO()
        qr_img.save(qr_buffer, format='PNG')
        qr_base64 = base64.b64encode(qr_buffer.getvalue()).decode('utf-8')

        test.ui.set_value('qr_code', f'data:image/png;base64,{qr_base64}')
    except ImportError:
        qr_fallback = Image.new('RGB', (300, 300), color='lightgray')
        qr_draw = ImageDraw.Draw(qr_fallback)
        qr_draw.text((50, 140), "QR code library not installed", fill='black')

        qr_buffer = io.BytesIO()
        qr_fallback.save(qr_buffer, format='PNG')
        qr_base64 = base64.b64encode(qr_buffer.getvalue()).decode('utf-8')

        test.ui.set_value('qr_code', f'data:image/png;base64,{qr_base64}')

    return test.ui.depends_on_input([])
