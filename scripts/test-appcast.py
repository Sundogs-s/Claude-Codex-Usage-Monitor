"""Validate generated Sparkle XML shape; cryptographic signing is tested on the Mac."""
import base64
import importlib.util
from pathlib import Path
import tempfile
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("appcast", Path(__file__).with_name("generate-appcast.py"))
appcast = importlib.util.module_from_spec(spec)
spec.loader.exec_module(appcast)
with tempfile.TemporaryDirectory(prefix="usage-monitor-appcast-") as root:
    root = Path(root)
    archive = root / "UsageMonitor-1.2.3-arm64-update.zip"
    archive.write_bytes(b"format-test-fixture-only")
    signature = root / "signature.xml"
    signature.write_text(f'sparkle:edSignature="{base64.b64encode(bytes(64)).decode()}" length="{archive.stat().st_size}"')
    for channel, name in [("stable", "appcast.xml"), ("preview", "appcast-preview.xml")]:
        output = appcast.generate(archive, signature, "1.2.3", "https://updates.example.test/usage", channel, root)
        assert output.name == name
        item = ET.parse(output).find("./channel/item")
        assert item.find(f"{{{appcast.SPARKLE}}}minimumSystemVersion").text == "13.0"
        assert item.find("enclosure").attrib["url"].endswith("/v1.2.3/" + archive.name)
    for url in ["http://host/", "file:///tmp/", "https://user:secret@host/", "https://host/path?token=x"]:
        try:
            appcast.generate(archive, signature, "1.2.3", url, "stable", root)
        except ValueError:
            pass
        else:
            raise AssertionError("Unsafe feed URL accepted")
    signature.write_text('sparkle:edSignature="invalid" length="0"')
    try:
        appcast.generate(archive, signature, "1.2.3", "https://host/", "stable", root)
    except ValueError:
        pass
    else:
        raise AssertionError("Bad signature metadata accepted")
print("PASS: Sparkle stable/preview XML, archive URL, minimum OS and invalid metadata rejection")
