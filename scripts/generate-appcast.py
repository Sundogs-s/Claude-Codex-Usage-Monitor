"""Generate a platform-specific Sparkle feed from sign_update output (no private keys)."""
import argparse
import base64
from datetime import datetime, timezone
from email.utils import format_datetime
from pathlib import Path
import re
from urllib.parse import quote, urlsplit
import xml.etree.ElementTree as ET

SPARKLE = "http://www.andymatuschak.org/xml-namespaces/sparkle"
ET.register_namespace("sparkle", SPARKLE)

def generate(archive, signature_file, version, base_url, channel, output_directory):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        raise ValueError("Invalid release version")
    parsed = urlsplit(base_url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("Update base URL must be HTTPS without credentials/query/fragment")
    archive, signature_file = Path(archive), Path(signature_file)
    if archive.name != f"UsageMonitor-{version}-arm64-update.zip" or not archive.is_file():
        raise ValueError("Unexpected update archive name")
    fragment = signature_file.read_text(encoding="utf-8").strip()
    signed = ET.fromstring(f'<enclosure xmlns:sparkle="{SPARKLE}" {fragment} />')
    signature = signed.attrib[f"{{{SPARKLE}}}edSignature"]
    if len(base64.b64decode(signature, validate=True)) != 64 or int(signed.attrib["length"]) != archive.stat().st_size:
        raise ValueError("Signature/length is invalid")
    if channel not in ("stable", "preview"):
        raise ValueError("Unsupported channel")
    rss = ET.Element("rss", version="2.0")
    feed = ET.SubElement(rss, "channel")
    ET.SubElement(feed, "title").text = f"Usage Monitor macOS arm64 ({channel})"
    item = ET.SubElement(feed, "item")
    ET.SubElement(item, "title").text = f"Usage Monitor {version}"
    ET.SubElement(item, f"{{{SPARKLE}}}version").text = version
    ET.SubElement(item, f"{{{SPARKLE}}}shortVersionString").text = version
    ET.SubElement(item, f"{{{SPARKLE}}}minimumSystemVersion").text = "13.0"
    ET.SubElement(item, "pubDate").text = format_datetime(datetime.now(timezone.utc), usegmt=True)
    ET.SubElement(item, "enclosure", {
        "url": base_url.rstrip("/") + "/v" + version + "/" + quote(archive.name),
        f"{{{SPARKLE}}}edSignature": signature, "length": str(archive.stat().st_size),
        "type": "application/octet-stream",
    })
    output = Path(output_directory)
    output.mkdir(parents=True, exist_ok=True)
    name = "appcast-preview.xml" if channel == "preview" else "appcast.xml"
    target = output / name
    temporary = output / (name + ".partial")
    ET.indent(rss)
    ET.ElementTree(rss).write(temporary, encoding="utf-8", xml_declaration=True)
    temporary.replace(target)
    return target

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("archive", "signature", "version", "base-url", "output-directory"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), default="stable")
    args = parser.parse_args()
    print(generate(args.archive, args.signature, args.version, args.base_url, args.channel, args.output_directory))
