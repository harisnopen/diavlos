"""Stitch the two desktops side by side, one frame per 1.5 s beat, 2 fps.

    python3 stitch.py FRAMES_A FRAMES_B OUT_DIR NAME "LABEL A" "LABEL B"

Writes NAME.mp4 and NAME.gif here.
Needs Pillow and imageio-ffmpeg.
"""
import os, sys, datetime, subprocess
from PIL import Image, ImageDraw, ImageFont
import imageio_ffmpeg
FF = imageio_ffmpeg.get_ffmpeg_exe()
A, B, OUT, NAME, LA, LB = sys.argv[1:7]
def load(d):
    r = {}
    for f in sorted(os.listdir(d)):
        try:
            im = Image.open(os.path.join(d, f)); im.load(); r[int(f[:-4])] = im.convert("RGB")
        except Exception as e:
            print("skip", f, e)
    return r
fa, fb = load(A), load(B)
beats = sorted(set(fa) & set(fb))
# Stop 6 s after the last change on either screen.
# A blinking cursor is not a change: count only changes bigger than it.
from PIL import ImageChops
def big(x, y):
    b = ImageChops.difference(x.crop((0, 30, 1280, 710)), y.crop((0, 30, 1280, 710))).getbbox()
    return b is not None and (b[2] - b[0]) * (b[3] - b[1]) > 400
lastchg = beats[0]
for p, t in zip(beats, beats[1:]):
    if big(fa[p], fa[t]) or big(fb[p], fb[t]): lastchg = t
beats = [t for t in beats if t <= lastchg + 6000]
print(len(beats), "frames", datetime.datetime.utcfromtimestamp(beats[0]/1000), "to", datetime.datetime.utcfromtimestamp(beats[-1]/1000))
font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 22)
mono = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 22)
GAP, TOP = 8, 40
os.makedirs(OUT, exist_ok=True)
for i, t in enumerate(beats):
    W = 1280 * 2 + GAP
    fr = Image.new("RGB", (W, TOP + 680), (30, 30, 36))
    fr.paste(fa[t].crop((0, 30, 1280, 710)), (0, TOP))
    fr.paste(fb[t].crop((0, 30, 1280, 710)), (1280 + GAP, TOP))
    d = ImageDraw.Draw(fr)
    d.text((14, 8), LA, font=font, fill=(120, 220, 255))
    d.text((1280 + GAP + 14, 8), LB, font=font, fill=(255, 200, 120))
    clock = datetime.datetime.utcfromtimestamp(t / 1000).strftime("%H:%M:%S UTC")
    d.text((W - 14, 8), clock, font=mono, fill=(230, 230, 230), anchor="ra")
    fr.save(f"{OUT}/{i:04d}.png")
subprocess.run([FF, "-y", "-loglevel", "error", "-framerate", "2", "-i", f"{OUT}/%04d.png",
    "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "26", "-r", "2", "-movflags", "+faststart", NAME + ".mp4"], check=True)
subprocess.run([FF, "-y", "-loglevel", "error", "-framerate", "2", "-i", f"{OUT}/%04d.png", "-vf",
    "scale=1600:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];[b][p]paletteuse=dither=none",
    "-loop", "0", NAME + ".gif"], check=True)
