import sharp from 'sharp';
import { readFileSync, writeFileSync } from 'fs';

const src = 'src-tauri/icons/source.png';
const sizes = [16, 32, 48, 256];

async function makePng(size) {
  return sharp(src).resize(size, size).png().toBuffer();
}

async function main() {
  const pngs = await Promise.all(sizes.map(makePng));

  // ICO header
  const headerSize = 6 + 16 * sizes.length;
  const bufs = [Buffer.alloc(headerSize)];

  // ICONDIR
  bufs[0].writeUInt16LE(0, 0);           // reserved
  bufs[0].writeUInt16LE(1, 2);           // type = ICO
  bufs[0].writeUInt16LE(sizes.length, 4); // count

  let offset = headerSize;
  for (let i = 0; i < sizes.length; i++) {
    const s = sizes[i];
    const entry = bufs[0];
    const base = 6 + i * 16;
    entry[base + 0] = s === 256 ? 0 : s;   // width
    entry[base + 1] = s === 256 ? 0 : s;   // height
    entry[base + 2] = 0;                    // colorCount
    entry[base + 3] = 0;                    // reserved
    entry.writeUInt16LE(0, base + 4);       // planes
    entry.writeUInt16LE(32, base + 6);      // bitCount
    entry.writeUInt32LE(pngs[i].length, base + 8);
    entry.writeUInt32LE(offset, base + 12);
    offset += pngs[i].length;
    bufs.push(pngs[i]);
  }

  const ico = Buffer.concat(bufs);
  writeFileSync('src-tauri/icons/icon.ico', ico);
  console.log(`icon.ico created: ${ico.length} bytes`);

  // Also update 32x32 and 128x128 PNGs
  await sharp(src).resize(32, 32).png().toFile('src-tauri/icons/32x32.png');
  await sharp(src).resize(128, 128).png().toFile('src-tauri/icons/128x128.png');
  await sharp(src).resize(256, 256).png().toFile('src-tauri/icons/128x128@2x.png');
  console.log('PNGs updated');
}

main().catch(console.error);
