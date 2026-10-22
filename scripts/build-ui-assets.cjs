// Build-time only. Pass the path to an installed sharp package; no runtime dependency.
const fs = require('node:fs');
const path = require('node:path');
const sharp = require(process.argv[2] || 'sharp');
const root = path.resolve(__dirname, '..', 'assets', 'ui');
(async () => {
  for (const name of ['claude','codex','cursor','settings','repeat','external-link','close','check','log-out','desktop']) {
    const svg = fs.readFileSync(path.join(root, name + '.svg'), 'utf8')
      .replace(/currentColor/g, '#ffffff');
    const pipeline = sharp(Buffer.from(svg), { density: 384 }).resize(96,96).ensureAlpha();
    await pipeline.clone().png().toFile(path.join(root, name + '.png'));
    const {data,info} = await pipeline.raw().toBuffer({resolveWithObject:true});
    const mask = Buffer.alloc(96*96);
    for(let i=0;i<mask.length;i++) mask[i] = data[i*info.channels+3];
    fs.writeFileSync(path.join(root, name + '.mask'), mask);
  }
})().catch(error=>{console.error(error);process.exitCode=1;});
