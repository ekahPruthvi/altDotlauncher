<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Markdown Editor</title>
  <link rel="stylesheet" href="style.css">
  <script src="https://cdn.jsdelivr.net/npm/marked/marked.min.js"></script>
</head>
<body>
  <div class="container">
    <div class="sidebar">
      <h1>Markdown Editor</h1>
      
      <div class="search-container">
        <input type="text" id="search-input" placeholder="Search files...">
        <button id="search-button">Search</button>
      </div>
      
      <div id="search-results" class="search-results"></div>
      
      <div class="files-container">
        <h2>Files</h2>
        <div id="file-list" class="file-list">
          <!-- Files will be loaded here dynamically -->
        </div>
      </div>
      
      <button id="new-file-btn" class="new-file-btn">New File</button>
    </div>
    
    <div class="main-content">
      <div class="editor-header">
        <h2 id="current-filename">No File Selected</h2>
        <div class="editor-controls">
          <div class="tab-buttons">
            <button class="tab-button active" data-tab="write">Write</button>
            <button class="tab-button" data-tab="preview">Preview</button>
            <button class="tab-button" data-tab="split">Split</button>
          </div>
          <button id="save-button" class="save-button">Save</button>
        </div>
      </div>
      
      <div class="editor-container">
        <div id="write-tab" class="tab-content active">
          <textarea id="markdown-editor" placeholder="Start writing in Markdown..."></textarea>
        </div>
        
        <div id="preview-tab" class="tab-content">
          <div id="markdown-preview" class="markdown-content"></div>
        </div>
        
        <div id="split-tab" class="tab-content">
          <div class="split-pane">
            <textarea id="split-editor" placeholder="Start writing in Markdown..."></textarea>
          </div>
          <div class="split-pane">
            <div id="split-preview" class="markdown-content"></div>
          </div>
        </div>
      </div>
    </div>
  </div>
  
  <div id="toast" class="toast"></div>
  
  <script src="script.js"></script>
</body>
</html>