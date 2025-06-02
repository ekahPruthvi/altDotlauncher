// Global variables
let currentFile = '';
let editor = null;
let preview = null;
let currentDate = new Date();
let calendarData = {};

// Initialize the application
document.addEventListener('DOMContentLoaded', function() {
    editor = document.getElementById('editor');
    preview = document.getElementById('preview');
    
    // Set initial file
    const filename = document.getElementById('filename').value;
    if (filename) {
        currentFile = filename;
    }
    
    // Update preview on editor change
    editor.addEventListener('input', updatePreview);
    
    // Initial preview update
    updatePreview();
    
    // Initialize calendar
    loadCalendar();
    
    // Auto-save every 30 seconds
    setInterval(autoSave, 30000);
});

// Markdown to HTML converter
function markdownToHtml(markdown) {
    return markdown
        // Headers
        .replace(/^### (.*$)/gim, '<h3>$1</h3>')
        .replace(/^## (.*$)/gim, '<h2>$1</h2>')
        .replace(/^# (.*$)/gim, '<h1>$1</h1>')
        
        // Tables
        .replace(/^\|(.+)\|\s*$/gim, function(match, content) {
            const cells = content.split('|').map(cell => cell.trim());
            return '<tr>' + cells.map(cell => `<td>${cell}</td>`).join('') + '</tr>';
        })
        .replace(/(<tr>.*<\/tr>\s*)+/gim, function(match) {
            const rows = match.split('</tr>').filter(row => row.trim());
            if (rows.length > 1) {
                const headerRow = rows[0] + '</tr>';
                const bodyRows = rows.slice(1).map(row => row + '</tr>').join('');
                return `<table class="markdown-table"><thead>${headerRow}</thead><tbody>${bodyRows}</tbody></table>`;
            }
            return `<table class="markdown-table"><tbody>${match}</tbody></table>`;
        })
        
        // Bold
        .replace(/\*\*(.*)\*\*/gim, '<strong>$1</strong>')
        .replace(/__(.*?)__/gim, '<strong>$1</strong>')
        
        // Italic
        .replace(/\*(.*)\*/gim, '<em>$1</em>')
        .replace(/_(.*?)_/gim, '<em>$1</em>')
        
        // Code blocks
        .replace(/```([\s\S]*?)```/gim, '<pre><code>$1</code></pre>')
        
        // Inline code
        .replace(/`([^`]+)`/gim, '<code>$1</code>')
        
        // Links
        .replace(/\[([^\]]+)\]\(([^)]+)\)/gim, '<a href="$2" target="_blank">$1</a>')
        
        // Blockquotes
        .replace(/^> (.*$)/gim, '<blockquote>$1</blockquote>')
        
        // Unordered lists
        .replace(/^\* (.*$)/gim, '<li>$1</li>')
        .replace(/(<li>.*<\/li>)/gims, '<ul>$1</ul>')
        
        // Ordered lists
        .replace(/^\d+\. (.*$)/gim, '<li>$1</li>')
        
        // Horizontal rules
        .replace(/^---$/gim, '<hr>')
        
        // Line breaks
        .replace(/\n\n/gim, '</p><p>')
        .replace(/^(.*)$/gim, '<p>$1</p>')
        
        // Clean up empty paragraphs
        .replace(/<p><\/p>/gim, '')
        .replace(/<p>(<h[1-6]>.*<\/h[1-6]>)<\/p>/gim, '$1')
        .replace(/<p>(<ul>.*<\/ul>)<\/p>/gims, '$1')
        .replace(/<p>(<blockquote>.*<\/blockquote>)<\/p>/gim, '$1')
        .replace(/<p>(<pre><code>.*<\/code><\/pre>)<\/p>/gims, '$1')
        .replace(/<p>(<table.*<\/table>)<\/p>/gims, '$1')
        .replace(/<p><hr><\/p>/gim, '<hr>');
}

// Update preview
function updatePreview() {
    const markdown = editor.value;
    const html = markdownToHtml(markdown);
    preview.innerHTML = html;
}

// File operations
function openFile(filename) {
    window.location.href = `?file=${encodeURIComponent(filename)}`;
}

function saveFile() {
    const filename = document.getElementById('filename').value;
    const content = editor.value;
    
    if (!filename) {
        showToast('Please enter a filename', 'error');
        return;
    }
    
    if (!filename.endsWith('.md')) {
        document.getElementById('filename').value = filename + '.md';
    }
    
    const formData = new FormData();
    formData.append('action', 'save');
    formData.append('filename', document.getElementById('filename').value);
    formData.append('content', content);
    
    fetch('', {
        method: 'POST',
        body: formData
    })
    .then(response => response.json())
    .then(data => {
        if (data.success) {
            showToast(data.message, 'success');
            currentFile = document.getElementById('filename').value;
            setTimeout(() => {
                location.reload();
            }, 1000);
        } else {
            showToast(data.message, 'error');
        }
    })
    .catch(error => {
        showToast('Error saving file', 'error');
        console.error('Error:', error);
    });
}

function deleteFile(event, filename) {
    event.stopPropagation();
    
    if (!confirm(`Are you sure you want to delete "${filename}"?`)) {
        return;
    }
    
    const formData = new FormData();
    formData.append('action', 'delete');
    formData.append('filename', filename);
    
    fetch('', {
        method: 'POST',
        body: formData
    })
    .then(response => response.json())
    .then(data => {
        if (data.success) {
            showToast(data.message, 'success');
            setTimeout(() => {
                location.reload();
            }, 1000);
        } else {
            showToast(data.message, 'error');
        }
    })
    .catch(error => {
        showToast('Error deleting file', 'error');
        console.error('Error:', error);
    });
}

function createNewFile() {
    const filename = prompt('Enter filename (without .md extension):');
    if (!filename) return;
    
    const formData = new FormData();
    formData.append('action', 'create');
    formData.append('filename', filename);
    
    fetch('', {
        method: 'POST',
        body: formData
    })
    .then(response => response.json())
    .then(data => {
        if (data.success) {
            showToast(data.message, 'success');
            setTimeout(() => {
                window.location.href = `?file=${encodeURIComponent(data.filename)}`;
            }, 1000);
        } else {
            showToast(data.message, 'error');
        }
    })
    .catch(error => {
        showToast('Error creating file', 'error');
        console.error('Error:', error);
    });
}

// Tab switching functionality
function switchTab(tab) {
    // Remove active class from all tabs
    document.querySelectorAll('.tab-btn').forEach(btn => btn.classList.remove('active'));
    document.querySelectorAll('.tab-content').forEach(content => content.style.display = 'none');
    
    // Activate selected tab
    event.target.classList.add('active');
    document.getElementById(tab + 'Tab').style.display = 'block';
    
    if (tab === 'calendar') {
        loadCalendar();
    }
}

// Calendar functionality
function loadCalendar() {
    const year = currentDate.getFullYear();
    const month = currentDate.getMonth() + 1;
    
    document.getElementById('monthYear').textContent = 
        currentDate.toLocaleDateString('en-US', { month: 'long', year: 'numeric' });
    
    // Fetch calendar data
    const formData = new FormData();
    formData.append('action', 'get_calendar_data');
    formData.append('year', year);
    formData.append('month', month);
    
    fetch('', {
        method: 'POST',
        body: formData
    })
    .then(response => response.json())
    .then(data => {
        if (data.success) {
            calendarData = data.data;
            renderCalendar();
        }
    })
    .catch(error => {
        console.error('Error loading calendar data:', error);
    });
}

function renderCalendar() {
    const grid = document.getElementById('calendarGrid');
    grid.innerHTML = '';
    
    // Add day headers
    const dayHeaders = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
    dayHeaders.forEach(day => {
        const header = document.createElement('div');
        header.className = 'day-header';
        header.textContent = day;
        grid.appendChild(header);
    });
    
    const year = currentDate.getFullYear();
    const month = currentDate.getMonth();
    const firstDay = new Date(year, month, 1);
    const lastDay = new Date(year, month + 1, 0);
    const startDate = new Date(firstDay);
    startDate.setDate(startDate.getDate() - firstDay.getDay());
    
    const today = new Date();
    
    for (let i = 0; i < 42; i++) {
        const cellDate = new Date(startDate);
        cellDate.setDate(startDate.getDate() + i);
        
        const dayElement = document.createElement('div');
        dayElement.className = 'calendar-day';
        
        // Fix the date string format to match PHP date format
        const year = cellDate.getFullYear();
        const month = String(cellDate.getMonth() + 1).padStart(2, '0');
        const day = String(cellDate.getDate()).padStart(2, '0');
        const dateStr = `${year}-${month}-${day}`;
        
        const dayData = calendarData[dateStr];
        
        if (cellDate.getMonth() !== currentDate.getMonth()) {
            dayElement.classList.add('other-month');
        }
        
        if (cellDate.toDateString() === today.toDateString()) {
            dayElement.classList.add('today');
        }
        
        if (dayData && (dayData.files.length > 0 || dayData.notes.length > 0)) {
            dayElement.classList.add('has-content');
        }
        
        const dayNumber = document.createElement('div');
        dayNumber.className = 'day-number';
        dayNumber.textContent = cellDate.getDate();
        dayElement.appendChild(dayNumber);
        
        const indicators = document.createElement('div');
        indicators.className = 'day-indicators';
        
        if (dayData) {
            // Add file indicators
            for (let j = 0; j < Math.min(dayData.files.length, 3); j++) {
                const dot = document.createElement('div');
                dot.className = 'file-dot';
                indicators.appendChild(dot);
            }
            
            // Add note indicators
            for (let j = 0; j < Math.min(dayData.notes.length, 3); j++) {
                const noteIndicator = document.createElement('div');
                noteIndicator.className = 'note-indicator';
                indicators.appendChild(noteIndicator);
            }
        }
        
        dayElement.appendChild(indicators);
        dayElement.onclick = () => openDateModal(dateStr, dayData);
        grid.appendChild(dayElement);
    }
}

function changeMonth(direction) {
    currentDate.setMonth(currentDate.getMonth() + direction);
    loadCalendar();
}

function openDateModal(date, dayData) {
    const modal = document.getElementById('noteModal');
    const modalDate = document.getElementById('modalDate');
    const noteInput = document.getElementById('noteInput');
    const notesList = document.getElementById('notesList');
    const fileList = document.getElementById('modalFileList');
    
    modalDate.textContent = new Date(date).toLocaleDateString('en-US', { 
        weekday: 'long', 
        year: 'numeric', 
        month: 'long', 
        day: 'numeric' 
    });
    
    noteInput.value = '';
    noteInput.dataset.date = date;
    
    // Display notes for this date
    notesList.innerHTML = '';
    if (dayData && dayData.notes.length > 0) {
        const notesHeader = document.createElement('h4');
        notesHeader.textContent = 'Notes:';
        notesList.appendChild(notesHeader);
        
        dayData.notes.forEach((note, index) => {
            const noteItem = document.createElement('div');
            noteItem.className = 'note-item';
            noteItem.innerHTML = `
                <span class="note-text">${note}</span>
                <button class="delete-note-btn" onclick="deleteNote('${date}', ${index})">×</button>
            `;
            notesList.appendChild(noteItem);
        });
    }
    
    // Display files for this date
    fileList.innerHTML = '';
    if (dayData && dayData.files.length > 0) {
        const header = document.createElement('h4');
        header.textContent = 'Files created on this date:';
        fileList.appendChild(header);
        
        dayData.files.forEach(file => {
            const fileItem = document.createElement('div');
            fileItem.className = 'modal-file-item';
            fileItem.innerHTML = `<span class="file-icon">📄</span><span>${file}</span>`;
            fileItem.onclick = () => {
                closeNoteModal();
                openFile(file);
                switchTab('files');
            };
            fileList.appendChild(fileItem);
        });
    }
    
    modal.style.display = 'block';
}

function closeNoteModal() {
    document.getElementById('noteModal').style.display = 'none';
}

function addNote() {
    const noteInput = document.getElementById('noteInput');
    const date = noteInput.dataset.date;
    const note = noteInput.value.trim();
    
    if (!note) {
        showToast('Please enter a note', 'error');
        return;
    }
    
    const formData = new FormData();
    formData.append('action', 'save_note');
    formData.append('date', date);
    formData.append('note', note);
    
    fetch('', {
        method: 'POST',
        body: formData
    })
    .then(response => response.json())
    .then(data => {
        if (data.success) {
            showToast(data.message, 'success');
            noteInput.value = '';
            loadCalendar();
            // Refresh the modal with updated data
            const dayData = calendarData[date];
            if (!dayData.notes) dayData.notes = [];
            dayData.notes.push(note);
            openDateModal(date, dayData);
        } else {
            showToast(data.message, 'error');
        }
    })
    .catch(error => {
        showToast('Error saving note', 'error');
        console.error('Error:', error);
    });
}

function deleteNote(date, noteIndex) {
    if (!confirm('Are you sure you want to delete this note?')) {
        return;
    }
    
    const formData = new FormData();
    formData.append('action', 'delete_note');
    formData.append('date', date);
    formData.append('noteIndex', noteIndex);
    
    fetch('', {
        method: 'POST',
        body: formData
    })
    .then(response => response.json())
    .then(data => {
        if (data.success) {
            showToast(data.message, 'success');
            loadCalendar();
            // Refresh the modal with updated data
            const dayData = calendarData[date];
            if (dayData && dayData.notes) {
                dayData.notes.splice(noteIndex, 1);
            }
            openDateModal(date, dayData);
        } else {
            showToast(data.message, 'error');
        }
    })
    .catch(error => {
        showToast('Error deleting note', 'error');
        console.error('Error:', error);
    });
}

// Close modal when clicking outside
window.onclick = function(event) {
    const modal = document.getElementById('noteModal');
    if (event.target === modal) {
        closeNoteModal();
    }
}

// View toggle functionality
function toggleView(view) {
    const editorPane = document.getElementById('editorPane');
    const previewPane = document.getElementById('previewPane');
    const container = document.querySelector('.editor-container');
    
    // Remove all view classes
    container.classList.remove('editor-only', 'preview-only');
    
    // Remove active class from all buttons
    document.querySelectorAll('.view-toggle .btn').forEach(btn => btn.classList.remove('active'));
    
    // Add active class to clicked button
    document.getElementById(view + 'Btn').classList.add('active');
    
    // Apply view
    switch(view) {
        case 'editor':
            container.classList.add('editor-only');
            break;
        case 'preview':
            container.classList.add('preview-only');
            break;
        case 'split':
            // Default split view - no special classes needed
            break;
    }
}

// Auto-save functionality
function autoSave() {
    if (currentFile && editor.value.trim() !== '') {
        const formData = new FormData();
        formData.append('action', 'save');
        formData.append('filename', currentFile);
        formData.append('content', editor.value);
        
        fetch('', {
            method: 'POST',
            body: formData
        })
        .then(response => response.json())
        .then(data => {
            if (data.success) {
                console.log('Auto-saved successfully');
            }
        })
        .catch(error => {
            console.error('Auto-save error:', error);
        });
    }
}

// Toast notification system
function showToast(message, type = 'success') {
    const toast = document.getElementById('toast');
    toast.textContent = message;
    toast.className = `toast ${type}`;
    toast.classList.add('show');
    
    setTimeout(() => {
        toast.classList.remove('show');
    }, 3000);
}

// Keyboard shortcuts
document.addEventListener('keydown', function(e) {
    // Ctrl+S or Cmd+S to save
    if ((e.ctrlKey || e.metaKey) && e.key === 's') {
        e.preventDefault();
        saveFile();
    }
    
    // Ctrl+N or Cmd+N to create new file
    if ((e.ctrlKey || e.metaKey) && e.key === 'n') {
        e.preventDefault();
        createNewFile();
    }
});

// Handle tab key in editor for indentation
document.getElementById('editor').addEventListener('keydown', function(e) {
    if (e.key === 'Tab') {
        e.preventDefault();
        const start = this.selectionStart;
        const end = this.selectionEnd;
        
        // Insert tab character
        this.value = this.value.substring(0, start) + '\t' + this.value.substring(end);
        
        // Move cursor
        this.selectionStart = this.selectionEnd = start + 1;
        
        // Update preview
        updatePreview();
    }
});