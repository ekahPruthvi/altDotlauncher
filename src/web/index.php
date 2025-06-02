
<?php
$filesDir = 'markdown_files';
$notesDir = 'calendar_notes';

if (!file_exists($filesDir)) {
    mkdir($filesDir, 0777, true);
}

if (!file_exists($notesDir)) {
    mkdir($notesDir, 0777, true);
}

$files = [];
if (is_dir($filesDir)) {
    $files = array_filter(scandir($filesDir), function($file) {
        return pathinfo($file, PATHINFO_EXTENSION) === 'md';
    });
}

$currentFile = '';
$currentContent = '';

if (isset($_GET['file']) && $_GET['file']) {
    $currentFile = $_GET['file'];
    $filePath = $filesDir . '/' . $currentFile;
    if (file_exists($filePath)) {
        $currentContent = file_get_contents($filePath);
    }
}

// Handle POST requests for file operations
if ($_SERVER['REQUEST_METHOD'] === 'POST') {
    header('Content-Type: application/json');
    
    if (isset($_POST['action'])) {
        switch ($_POST['action']) {
            case 'save':
                $filename = $_POST['filename'];
                $content = $_POST['content'];
                $filePath = $filesDir . '/' . $filename;
                
                if (file_put_contents($filePath, $content) !== false) {
                    echo json_encode(['success' => true, 'message' => 'File saved successfully']);
                } else {
                    echo json_encode(['success' => false, 'message' => 'Failed to save file']);
                }
                break;
                
            case 'delete':
                $filename = $_POST['filename'];
                $filePath = $filesDir . '/' . $filename;
                
                if (file_exists($filePath) && unlink($filePath)) {
                    echo json_encode(['success' => true, 'message' => 'File deleted successfully']);
                } else {
                    echo json_encode(['success' => false, 'message' => 'Failed to delete file']);
                }
                break;
                
            case 'create':
                $filename = $_POST['filename'];
                if (!str_ends_with($filename, '.md')) {
                    $filename .= '.md';
                }
                $filePath = $filesDir . '/' . $filename;
                
                if (!file_exists($filePath)) {
                    if (file_put_contents($filePath, '') !== false) {
                        echo json_encode(['success' => true, 'message' => 'File created successfully', 'filename' => $filename]);
                    } else {
                        echo json_encode(['success' => false, 'message' => 'Failed to create file']);
                    }
                } else {
                    echo json_encode(['success' => false, 'message' => 'File already exists']);
                }
                break;
                
            case 'get_calendar_data':
                $year = $_POST['year'];
                $month = $_POST['month'];
                $calendarData = [];
                
                // Get file creation dates
                foreach ($files as $file) {
                    $filePath = $filesDir . '/' . $file;
                    $creationTime = filemtime($filePath);
                    $date = date('Y-m-d', $creationTime);
                    if (!isset($calendarData[$date])) {
                        $calendarData[$date] = ['files' => [], 'notes' => []];
                    }
                    $calendarData[$date]['files'][] = $file;
                }
                
                // Get notes for the month
                $notesFile = $notesDir . '/' . $year . '-' . str_pad($month, 2, '0', STR_PAD_LEFT) . '.json';
                if (file_exists($notesFile)) {
                    $notes = json_decode(file_get_contents($notesFile), true);
                    foreach ($notes as $date => $notesList) {
                        if (!isset($calendarData[$date])) {
                            $calendarData[$date] = ['files' => [], 'notes' => []];
                        }
                        $calendarData[$date]['notes'] = $notesList;
                    }
                }
                
                echo json_encode(['success' => true, 'data' => $calendarData]);
                break;
                
            case 'save_note':
                $date = $_POST['date'];
                $note = $_POST['note'];
                $year = date('Y', strtotime($date));
                $month = date('m', strtotime($date));
                
                $notesFile = $notesDir . '/' . $year . '-' . $month . '.json';
                $notes = [];
                
                if (file_exists($notesFile)) {
                    $notes = json_decode(file_get_contents($notesFile), true);
                }
                
                if (!isset($notes[$date])) {
                    $notes[$date] = [];
                }
                
                if (!empty($note)) {
                    $notes[$date][] = $note;
                }
                
                if (file_put_contents($notesFile, json_encode($notes)) !== false) {
                    echo json_encode(['success' => true, 'message' => 'Note added successfully']);
                } else {
                    echo json_encode(['success' => false, 'message' => 'Failed to save note']);
                }
                break;
                
            case 'delete_note':
                $date = $_POST['date'];
                $noteIndex = $_POST['noteIndex'];
                $year = date('Y', strtotime($date));
                $month = date('m', strtotime($date));
                
                $notesFile = $notesDir . '/' . $year . '-' . $month . '.json';
                
                if (file_exists($notesFile)) {
                    $notes = json_decode(file_get_contents($notesFile), true);
                    
                    if (isset($notes[$date]) && isset($notes[$date][$noteIndex])) {
                        array_splice($notes[$date], $noteIndex, 1);
                        
                        if (empty($notes[$date])) {
                            unset($notes[$date]);
                        }
                        
                        if (file_put_contents($notesFile, json_encode($notes)) !== false) {
                            echo json_encode(['success' => true, 'message' => 'Note deleted successfully']);
                        } else {
                            echo json_encode(['success' => false, 'message' => 'Failed to delete note']);
                        }
                    } else {
                        echo json_encode(['success' => false, 'message' => 'Note not found']);
                    }
                } else {
                    echo json_encode(['success' => false, 'message' => 'Notes file not found']);
                }
                break;
        }
    }
    exit;
}
?>

<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>alt ● marker</title>
    <link rel="stylesheet" href="styles.css">
    <link rel="icon" type="image/x-icon" href="favicon.ico">
</head>
<body>
    <div class="app-container">
        <!-- Sidebar -->
        <div class="sidebar">
            <div class="sidebar-header">
                <h2>alt ● marker</h2>
            </div>
            
            <div class="sidebar-tabs">
                <button class="tab-btn active" onclick="switchTab('files')">Files</button>
                <button class="tab-btn" onclick="switchTab('calendar')">Calendar</button>
            </div>
            
            <div class="tab-content" id="filesTab">

                <div class="file-list">
                    <h3>markers</h3>
                    <div class="files" id="filesList">
                        <?php foreach ($files as $file): ?>
                            <div class="file-item <?= $file === $currentFile ? 'active' : '' ?>" 
                                 onclick="openFile('<?= htmlspecialchars($file) ?>')">
                                <span class="file-name"><?= htmlspecialchars($file) ?></span>
                                <button class="delete-btn" onclick="deleteFile(event, '<?= htmlspecialchars($file) ?>')">Del</button>
                            </div>
                        <?php endforeach; ?>
                    </div>
                </div>
                
            </div>
            
            <div class="tab-content" id="calendarTab" style="display: none;">
                <div class="calendar-container">
                    <div class="calendar-header">
                        <button class="nav-btn" onclick="changeMonth(-1)">❮</button>
                        <h3 id="monthYear"></h3>
                        <button class="nav-btn" onclick="changeMonth(1)">❯</button>
                    </div>
                    <div class="calendar-grid" id="calendarGrid"></div>
                </div>
            </div>

        </div>

        <!-- Main Content -->
        <div class="main-content">

            <div class="editor-container">
                <div class="editor-pane" id="editorPane">
                    <textarea id="editor" placeholder="Start writing your markdown..."><?= htmlspecialchars($currentContent) ?></textarea>
                </div>
                <div class="preview-pane" id="previewPane">
                    <div id="preview" class="markdown-preview"></div>
                </div>
            </div>

            <div class="file-info">
                <input type="text" id="filename" value="<?= htmlspecialchars($currentFile) ?>" placeholder="untitled.md">
                
                <button class="btn btn-primary" onclick="createNewFile()">
                    New Mark
                </button>

                <div class="dummy"></div>

                <div class="view-toggle">
                    <button class="btn" onclick="toggleView('editor')" id="editorBtn">Editor</button>
                    <button class="btn" onclick="toggleView('preview')" id="previewBtn">Preview</button>
                    <button class="btn active" onclick="toggleView('split')" id="splitBtn">Split</button>
                </div>

            </div>

        </div>
        
    </div>

    <!-- Toast Notifications -->
    <div id="toast" class="toast"></div>

    <!-- Note Modal -->
    <div id="noteModal" class="modal">
        <div class="modal-content">
            <div class="modal-header">
                <h3 id="modalDate"></h3>
                <span class="close" onclick="closeNoteModal()">&times;</span>
            </div>
            <div class="modal-body">
                <div class="note-input-section">
                    <input type="text" id="noteInput" placeholder="Add a note for this date..." maxlength="100">
                    <button class="btn btn-success" onclick="addNote()">Add Note</button>
                </div>
                <div class="notes-list" id="notesList"></div>
                <div class="file-list-modal" id="modalFileList"></div>
            </div>
            <div class="modal-footer">
                <button class="btn" onclick="closeNoteModal()">Close</button>
            </div>
        </div>
    </div>

    <script src="script.js"></script>
</body>
</html>