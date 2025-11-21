import base64
import io
from PIL import Image, ImageDraw

def run(log, ui):
    base_img = Image.new('RGB', (400, 300))
    draw = ImageDraw.Draw(base_img)

    for y in range(300):
        for x in range(400):
            r = int(255 * (x / 400))
            g = int(255 * (y / 300))
            b = 128
            base_img.putpixel((x, y), (r, g, b))

    draw.text((150, 130), "PNG Format", fill='white')
    draw.text((120, 160), "(Lossless Compression)", fill='white')

    png_buffer = io.BytesIO()
    base_img.save(png_buffer, format='PNG', optimize=True)
    png_base64 = base64.b64encode(png_buffer.getvalue()).decode('utf-8')

    ui.set_value('png_image', f'data:image/png;base64,{png_base64}')

    jpeg_img = base_img.copy()
    jpeg_draw = ImageDraw.Draw(jpeg_img)

    jpeg_draw.rectangle([0, 0, 400, 300], fill=None, outline='black', width=0)
    jpeg_draw.text((145, 130), "JPEG Format", fill='white')
    jpeg_draw.text((100, 160), "(Lossy Compression, Quality=85)", fill='white')

    jpeg_buffer = io.BytesIO()
    jpeg_img.save(jpeg_buffer, format='JPEG', quality=85)
    jpeg_base64 = base64.b64encode(jpeg_buffer.getvalue()).decode('utf-8')

    ui.set_value('jpeg_image', f'data:image/jpeg;base64,{jpeg_base64}')

    log.info(f"PNG size: {len(png_buffer.getvalue())} bytes")
    log.info(f"JPEG size: {len(jpeg_buffer.getvalue())} bytes")

    return ui.depends_on_input([])
