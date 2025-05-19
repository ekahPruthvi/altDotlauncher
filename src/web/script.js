document.addEventListener('DOMContentLoaded', function() {
  // DOM elements
  const fileList = document.getElementById('file-list');
  const markdownEditor = document.getElementById('markdown-editor');
  const splitEditor = document.getElementById('split-editor');
  const markdownPreview = document.getElementById('markdown-preview');
  const splitPreview = document.getElementById('split-preview');
  const currentFilename = document.getElementById('current-filename');
  const saveButton = document.getElementById('save-button');
  const newFileBtn = document.getElementById('new-file-btn');
  const searchInput = document.getElementById('search-input');
  const searchButton = document.getElementById('search-button');
  const searchResults = document.getElementById('search-results');
  const tabButtons = document.querySelectorAll('.tab-button');
  const tabContents = document.querySelectorAll('.tab-content');
  const toast = document.getElementById('toast');

  // State
  let currentFile = null;

  // Initialize
  fetchFileList();

  // Event listeners
  markdownEditor.addEventListener('input', updatePreview);
  splitEditor.addEventListener('input', updateSplitPreview);
  saveButton.addEventListener('click', saveCurrentFile);
  newFileBtn.addEventListener('click', createNewFile);
  searchButton.addEventListener('click', performSearch);
  
  // Tab switching
  tabButtons.forEach(button => {
    button.addEventListener('click', () => {
      const tab = button.dataset.tab;
      
      // Update active tab button
      tabButtons.forEach(btn => btn.classList.remove('active'));
      button.classList.add('active');
      
      // Update active tab content
      tabContents.forEach(content => content.classList.remove('active'));
      document.getElementById(`${tab}-tab`).classList.add('active');
    });
  });

  // Functions
  function fetchFileList() {
    fetch('api/list-files.php')
      .then(response => response.json())
      .then(data => {
        if (data.files) {
          renderFileList(data.files);
        }
      })
      .catch(error => {
        console.error('Error fetching file list:', error);
        showToast('Error loading files', 'error');
      });
  }

  function renderFileList(files) {
    fileList.innerHTML = '';
    
    if (files.length === 0) {
      fileList.innerHTML = '<div class="empty-message">No files found</div>';
      return;
    }
    
    files.forEach(file => {
      const fileItem = document.createElement('div');
      fileItem.className = 'file-item';
      fileItem.textContent = file;
      
      if (currentFile === file) {
        fileItem.classList.add('active');
      }
      
      fileItem.addEventListener('click', () => loadFile(file));
      
      fileList.appendChild(fileItem);
    });
  }

  function loadFile(filename) {
    fetch(`api/get-file.php?file=${encodeURIComponent(filename)}`)
      .then(response => response.json())
      .then(data => {
        if (data.content !== undefined) {
          markdownEditor.value = data.content;
          splitEditor.value = data.content;
          updatePreview();
          updateSplitPreview();
          
          currentFile = filename;
          currentFilename.textContent = filename;
          
          // Update active file in list
          document.querySelectorAll('.file-item').forEach(item => {
            item.classList.toggle('active', item.textContent === filename);
          });
        }
      })
      .catch(error => {
        console.error('Error loading file:', error);
        showToast('Error loading file', 'error');
      });
  }

  function updatePreview() {
    markdownPreview.innerHTML = marked.parse(markdownEditor.value);
  }

  function updateSplitPreview() {
    splitPreview.innerHTML = marked.parse(splitEditor.value);
  }

  function saveCurrentFile() {
    if (!currentFile) {
      createNewFile();
      return;
    }

    const content = markdownEditor.value;

    fetch('api/save-file.php', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        filename: currentFile,
        content: content
      })
    })
    .then(response => response.json())
    .then(data => {
      if (data.success) {
        showToast('File saved successfully', 'success');
        fetchFileList();
      } else {
        showToast(data.message || 'Error saving file', 'error');
      }
    })
    .catch(error => {
      console.error('Error saving file:', error);
      showToast('Error saving file', 'error');
    });
  }

  function createNewFile() {
    const filename = prompt("Enter a name for the new file (without .md extension):");
    if (!filename) return;

    const newFilename = `${filename}.md`;
    const content = markdownEditor.value || '# New File';

    fetch('api/save-file.php', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        filename: newFilename,
        content: content
      })
    })
    .then(response => response.json())
    .then(data => {
      if (data.success) {
        currentFile = newFilename;
        currentFilename.textContent = newFilename;
        showToast('File created successfully', 'success');
        fetchFileList();
      } else {
        showToast(data.message || 'Error creating file', 'error');
      }
    })
    .catch(error => {
      console.error('Error creating file:', error);
      showToast('Error creating file', 'error');
    });
  }

  function performSearch() {
    const query = searchInput.value.trim();
    
    if (!query) {
      searchResults.style.display = 'none';
      return;
    }
    
    fetch(`api/search-files.php?query=${encodeURIComponent(query)}`)
      .then(response => response.json())
      .then(data => {
        if (data.results) {
          renderSearchResults(data.results);
        } else {
          searchResults.innerHTML = '<div class="empty-message">No results found</div>';
          searchResults.style.display = 'block';
        }
      })
      .catch(error => {
        console.error('Error searching files:', error);
        showToast('Error searching files', 'error');
      });
  }

  function renderSearchResults(results) {
    searchResults.innerHTML = '';
    
    if (results.length === 0) {
      searchResults.innerHTML = '<div class="empty-message">No results found</div>';
      searchResults.style.display = 'block';
      return;
    }
    
    const clearButton = document.createElement('button');
    clearButton.className = 'clear-search-button';
    clearButton.textContent = 'Clear results';
    clearButton.addEventListener('click', () => {
      searchResults.style.display = 'none';
      searchInput.value = '';
    });
    searchResults.appendChild(clearButton);
    
    results.forEach(result => {
      const resultItem = document.createElement('div');
      resultItem.className = 'search-result-item';
      
      const title = document.createElement('div');
      title.className = 'search-result-item-title';
      title.textContent = result.file;
      
      const snippet = document.createElement('div');
      snippet.className = 'search-result-item-snippet';
      snippet.textContent = result.content;
      
      resultItem.appendChild(title);
      resultItem.appendChild(snippet);
      
      resultItem.addEventListener('click', () => {
        loadFile(result.file);
        searchResults.style.display = 'none';
      });
      
      searchResults.appendChild(resultItem);
    });
    
    searchResults.style.display = 'block';
  }

  function showToast(message, type = '') {
    toast.textContent = message;
    toast.className = 'toast';
    
    if (type) {
      toast.classList.add(type);
    }
    
    toast.classList.add('show');
    
    setTimeout(() => {
      toast.classList.remove('show');
    }, 3000);
  }
});