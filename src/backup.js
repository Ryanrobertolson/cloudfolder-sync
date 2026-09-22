const { google } = require('googleapis');
const fs = require('fs');
const path = require('path');
const mime = require('mime-types');
const config = require('./config');

const CONCURRENT_UPLOADS = 5;

async function backupFiles(auth) {
  const drive = google.drive({ version: 'v3', auth });
  const sourceDir = config.SOURCE_DIR;
  
  if (!fs.existsSync(sourceDir)) {
    throw new Error(`Source directory does not exist: ${sourceDir}`);
  }

  const folderId = await getOrCreateFolder(drive, config.DRIVE_FOLDER_NAME);
  const files = getAllFiles(sourceDir);
  
  console.log(`Found ${files.length} files to backup`);
  console.log(`Uploading with ${CONCURRENT_UPLOADS} concurrent streams...`);

  // Process files in parallel batches
  for (let i = 0; i < files.length; i += CONCURRENT_UPLOADS) {
    const batch = files.slice(i, i + CONCURRENT_UPLOADS);
    const promises = batch.map(filePath => uploadFile(drive, filePath, folderId, sourceDir));
    await Promise.all(promises);
    console.log(`Progress: ${Math.min(i + CONCURRENT_UPLOADS, files.length)}/${files.length} files uploaded`);
  }
}

function getAllFiles(dirPath, filesList = []) {
  const entries = fs.readdirSync(dirPath, { withFileTypes: true });
  
  for (const entry of entries) {
    const fullPath = path.join(dirPath, entry.name);
    if (entry.isDirectory()) {
      getAllFiles(fullPath, filesList);
    } else {
      filesList.push(fullPath);
    }
  }
  
  return filesList;
}

async function getOrCreateFolder(drive, folderName) {
  const response = await drive.files.list({
    q: `name='${folderName}' and mimeType='application/vnd.google-apps.folder' and trashed=false`,
    fields: 'files(id, name)',
  });

  if (response.data.files.length > 0) {
    console.log(`Using existing folder: ${folderName}`);
    return response.data.files[0].id;
  }

  const folderMetadata = {
    name: folderName,
    mimeType: 'application/vnd.google-apps.folder',
  };

  const folder = await drive.files.create({
    resource: folderMetadata,
    fields: 'id',
  });

  console.log(`Created new folder: ${folderName}`);
  return folder.data.id;
}

async function uploadFile(drive, filePath, folderId, baseDir) {
  const fileName = path.relative(baseDir, filePath);
  const mimeType = mime.lookup(filePath) || 'application/octet-stream';
  
  const fileMetadata = {
    name: fileName,
    parents: [folderId],
  };

  const fileSize = fs.statSync(filePath).size;
  
  const media = {
    mimeType: mimeType,
    body: fs.createReadStream(filePath, { highWaterMark: 16 * 1024 * 1024 }),
  };

  try {
    console.log(`Uploading: ${fileName} (${formatSize(fileSize)})`);
    
    const startTime = Date.now();
    
    const response = await drive.files.create({
      resource: fileMetadata,
      media: media,
      fields: 'id, name, size',
    });

    const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
    const speed = fileSize > 0 ? formatSize(fileSize / (elapsed || 1)) + '/s' : '';
    console.log(`Uploaded: ${response.data.name} (ID: ${response.data.id}) in ${elapsed}s ${speed}`);
    return response.data;
  } catch (error) {
    console.error(`Failed to upload ${fileName}:`, error.message);
    throw error;
  }
}

function formatSize(bytes) {
  const units = ['B', 'KB', 'MB', 'GB'];
  let size = bytes;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex++;
  }
  return `${size.toFixed(2)} ${units[unitIndex]}`;
}

module.exports = { backupFiles };
