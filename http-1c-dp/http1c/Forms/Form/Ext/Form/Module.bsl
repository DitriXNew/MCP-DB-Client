

// ============================================================================
// MCP Server Demo — 1C:Enterprise External Data Processor
// ============================================================================
//
// This module demonstrates how to use the http1c native component to build
// a fully-featured MCP (Model Context Protocol) server from 1C:Enterprise.
//
// Architecture:
//   Native DLL (HttpServer)  <->  1C BSL (this module)  <->  MCP Client (VS Code)
//
// The native component handles:
//   - HTTP transport (Streamable HTTP on localhost)
//   - JSON-RPC 2.0 message framing
//   - MCP session management (Mcp-Session-Id)
//   - Security (Origin validation, Bearer auth, rate limiting)
//   - SSE streaming for progress notifications
//
// This 1C module handles:
//   - Registering MCP tools, resources, and prompts
//   - Processing tool calls, resource reads, and prompt gets
//   - Business logic execution in the 1C context
//
// Usage as a Constructor Pattern:
//   1. Create an instance of the component
//   2. Register tools with RegisterTools() - JSON array of tool definitions
//   3. Register resources with RegisterResources() - JSON array of resource defs
//   4. Register prompts with RegisterPrompts() - JSON array of prompt defs
//   5. Start listening with StartListen(port)
//   6. Handle ExternalEvents: ToolCall, ResourceRead, PromptGet
//   7. Send results back with SendResponse()
//
// Each tool/resource/prompt can be added independently. The component
// supports dynamic registration - call RegisterTools() again at any time
// with an updated list (the MCP client will be notified via listChanged).
//
// ============================================================================

#Region Variables

&AtClient
Var Component;

&AtClient
Var AddInPath;

&AtClient
Var RuntimeStatus;

&AtClient
Var DefaultLogPath;

&AtClient
Var ScreenshotDataArray;

&AtClient
Var ScreenshotCurrentIndex;

&AtClient
Var SelfTestCtx;

&AtClient
Var SelfTestWaitTicks;

&AtClient
Var RawFileList;

&AtClient
Var RawFileIndex;

&AtClient
Var ScenAll;

&AtClient
Var ScenSkip;

&AtClient
Var ScenBatchNo;

#EndRegion


#Region FormEventHandlers

// ---------------------------------------------------------------------------
// ExternalEvent - main entry point for all events from the native component.
//
// The native DLL sends events via ExternalEvent mechanism:
//   Source = "HttpServer"
//   Event  = "ToolCall"     - MCP tools/call request
//          | "ResourceRead" - MCP resources/read request
//          | "PromptGet"    - MCP prompts/get request
//          | "Request"      - Legacy HTTP request (non-MCP)
//   Data   = JSON string with request details
// ---------------------------------------------------------------------------
&AtClient
Procedure ExternalEvent(Source, Event, Data)
	
	If Source <> "HttpServer" Then
		Return;
	EndIf;
	
	If Event = "ToolCall" Then
		ProcessToolCall(Data);
	ElsIf Event = "ResourceRead" Then
		ProcessResourceRead(Data);
	ElsIf Event = "PromptGet" Then
		ProcessPromptGet(Data);
	ElsIf Event = "Request" Then
		ProcessLegacyRequest(Data);
	Else
		Return;
	EndIf;

EndProcedure

// ---------------------------------------------------------------------------
// OnOpen — prefill the RAG demo fields with sensible defaults.
// ---------------------------------------------------------------------------
&AtClient
Procedure OnOpen(Cancel)

	// TEMP TRACE: confirm OnOpen fires at all + capture the launch parameter.
	Try
		TraceWriter = New TextWriter("D:\GitHub\MCP-DB-Client\rust-core\target\onopen-trace.txt", TextEncoding.UTF8);
		TraceWriter.Write("OnOpen fired " + String(CurrentDate())
			+ " build=" + BuildVersion() + Chars.LF
			+ "LaunchParameter=[" + String(LaunchParameter) + "]");
		TraceWriter.Close();
	Except
	EndTry;

	EnsureRagDefaults();

	// Attach the component on open (attach-only — NO InstallAddIn, which pops a
	// modal "external component installed" dialog and blocks headless runs).
	// Attaching from the bundle loads the component from a session-temp COPY that
	// holds only libhttp1cWin.dll, so rcore.dll (a plain dependency DLL) is not
	// beside it -> rag_not_installed. For the headless RAG self-test we instead
	// attach the on-disk DLL in ExtCompT IN-PLACE (full component already
	// installed there: libhttp1cWin.dll + rcore.dll + DirectML.dll side by side),
	// so rcore.dll is found beside it and real search is enabled. The shipping
	// path keeps the bundle attach (lite).
	LPOpen = "";
	Try
		LPOpen = Lower(String(LaunchParameter));
	Except
	EndTry;
	If StrFind(LPOpen, "ragselftest") > 0 Then
		AttachSelfTestComponent(0);
	Else
		AddInPath = GetDefaultAddInSource();
		BeginAttachingAddIn(New NotifyDescription("OnOpenAttachEnd", ThisObject),
			AddInPath, "http1c", AddInType.Native);
	EndIf;

EndProcedure

&AtClient
Function SelfTestAttachCandidates()
	// In-place attach locations for the headless self-test, tried in order until
	// one connects. The full component is installed flat in ExtCompT, so loading
	// the DLL there in-place puts rcore.dll right beside it.
	L = New Array;
	L.Add("C:\Users\DitriX\AppData\Roaming\1C\1cv8\ExtCompT\libhttp1cWin.dll");
	L.Add("C:\Users\DitriX\AppData\Roaming\1C\1cv8\ExtCompT");
	Return L;
EndFunction

&AtClient
Procedure AttachSelfTestComponent(Index)
	Cands = SelfTestAttachCandidates();
	If Index >= Cands.Count() Then
		TraceLine("all self-test attach candidates failed");
		Return;
	EndIf;
	AddInPath = Cands[Index];
	TraceLine("self-test attach attempt " + String(Index) + " -> " + AddInPath);
	BeginAttachingAddIn(New NotifyDescription("OnOpenAttachEnd", ThisObject, Index),
		AddInPath, "http1c", AddInType.Native);
EndProcedure

&AtClient
Function BuildVersion()
	// Bump on EVERY source change so the log proves a fresh .epf is running.
	Return "selftest-build-12-cap100-filters";
EndFunction

&AtClient
Function Ms()
	Return CurrentUniversalDateInMilliseconds();
EndFunction

&AtClient
Procedure TraceLine(Text)
	Try
		W = New TextWriter("D:\GitHub\MCP-DB-Client\rust-core\target\onopen-trace.txt",
			TextEncoding.UTF8, , True);
		W.WriteLine("" + Text);
		W.Close();
	Except
	EndTry;
EndProcedure

&AtClient
Procedure OnOpenAttachEnd(Connected, AdditionalParameters) Export

	TraceLine("OnOpenAttachEnd Connected=" + String(Connected) + " source=" + String(AddInPath));
	If Not Connected Then
		// Self-test attaches pass the candidate index; try the next location.
		If TypeOf(AdditionalParameters) = Type("Number") Then
			AttachSelfTestComponent(AdditionalParameters + 1);
		EndIf;
		Return;
	EndIf;
	Try
		Component = New("AddIn.http1c.HttpServer");
		Component.LoggingEnabled = EnableLogging;
		Component.LogPath = LogPath;
		Component.Timeout = 120;
		TraceLine("Component created, version=" + String(Component.Version));
	Except
		TraceLine("Component create EXCEPTION: " + ErrorDescription());
		Return;
	EndTry;

	// If launched for the headless self-test, run the RAG chain now that the
	// component is attached.
	LP = "";
	Try
		LP = Lower(String(LaunchParameter));
	Except
	EndTry;
	If StrFind(LP, "ragselftest") > 0 Then
		TraceLine("scheduling RunRagSelfTestDeferred");
		AttachIdleHandler("RunRagSelfTestDeferred", 1, True);
	EndIf;

EndProcedure

#EndRegion


#Region FormCommandHandlers

&AtClient
Procedure Connect(Command)
	
	EnsureLoggingDefaults();

	// Attach-only (no InstallAddIn — it pops a modal install dialog). The
	// component ships in the declared template bundle, so attach loads it directly.
	AddInPath = GetDefaultAddInSource();

	BeginAttachingAddIn(
		New NotifyDescription("AttachAddInEnd", ThisObject),
		AddInPath, "http1c", AddInType.Native);

EndProcedure

&AtClient
Procedure Disconnect(Command)
	
	If Component <> Undefined Then
		Component.BeginCallingStopListen(
			New NotifyDescription("StopListenEnd", ThisObject));
	EndIf;
	
EndProcedure

&AtClient
Procedure StopListenEnd(ResultCall, ParametersCall, AdditionalParameters) Export
	
	Component = Undefined;
	ResetRuntimeStatus();
	ShowMessageBox(, "Server stopped.");
	
EndProcedure

&AtClient
Procedure GetStatus(Command)
	
	If Component = Undefined Then
		ShowMessageBox(, "Component is not connected.");
		Return;
	EndIf;
	
	BuildCombinedStatusJSON(
		New NotifyDescription("GetStatusShowEnd", ThisObject));
	
EndProcedure

&AtClient
Procedure GetStatusShowEnd(StatusJson, AdditionalParameters) Export
	
	ShowMessageBox(, StatusJson);
	
EndProcedure

&AtClient
Procedure TakeScreenshot(Command)
	
	If Component = Undefined Then
		ShowMessageBox(, "Component is not connected.");
		Return;
	EndIf;
	
	PID = ScreenshotPID;
	Format = ScreenshotFormat;
	If Not ValueIsFilled(Format) Then
		Format = "jpeg";
	EndIf;
	Quality = ScreenshotQuality;
	If Quality = 0 Then
		Quality = 80;
	EndIf;
	
	Try
		Component.BeginCallingTakeScreenshot(
			New NotifyDescription("TakeScreenshotFormEnd", ThisObject),
				PID, Format, Quality, ScreenshotGrayscale);
	Except
		ShowMessageBox(, "Screenshot error: " + ErrorDescription());
	EndTry;
EndProcedure

&AtClient
Procedure TakeScreenshotFormEnd(ResultJson, ParametersCall, AdditionalParameters) Export
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(ResultJson);
		CaptureResult = ReadJSON(JSONReader, True);
	Except
		ShowMessageBox(, "Failed to parse result: " + ErrorDescription());
		Return;
	EndTry;
	
	Windows = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		Windows = CaptureResult["windows"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("windows", Windows);
	EndIf;
	
	ScreenshotDataArray = New Array;
	ScreenshotCurrentIndex = 0;
	
	If Windows = Undefined Or Windows.Count() = 0 Then
		CaptureError = "";
		If TypeOf(CaptureResult) = Type("Map") Then
			CaptureError = CaptureResult["error"];
		ElsIf TypeOf(CaptureResult) = Type("Structure") Then
			CaptureResult.Property("error", CaptureError);
		EndIf;
		If Not ValueIsFilled(CaptureError) Then
			CaptureError = "No windows captured";
		EndIf;
		ScreenshotInfo = CaptureError;
		ScreenshotPicture = "";
		Return;
	EndIf;
	
	For Each Win In Windows Do
		Title = "";
		ImageData = "";
		IsModal = False;
		IsMainWin = False;
		WinWidth = 0;
		WinHeight = 0;
		WinOwnerIndex = -1;
		WinLevel = 0;
		WinIsEnabled = True;
		WinIsMinimized = False;
		WinIsMaximized = False;
		WinZOrder = 0;
		
		If TypeOf(Win) = Type("Map") Then
			Title = Win["title"];
			ImageData = Win["image"];
			IsModal = Win["isModal"];
			IsMainWin = Win["isMainWindow"];
			WinWidth = Win["width"];
			WinHeight = Win["height"];
			WinOwnerIndex = Win["ownerIndex"];
			WinLevel = Win["level"];
			WinIsEnabled = Win["isEnabled"];
			WinIsMinimized = Win["isMinimized"];
			WinIsMaximized = Win["isMaximized"];
			WinZOrder = Win["zOrder"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("title", Title);
			Win.Property("image", ImageData);
			Win.Property("isModal", IsModal);
			Win.Property("isMainWindow", IsMainWin);
			Win.Property("width", WinWidth);
			Win.Property("height", WinHeight);
			Win.Property("ownerIndex", WinOwnerIndex);
			Win.Property("level", WinLevel);
			Win.Property("isEnabled", WinIsEnabled);
			Win.Property("isMinimized", WinIsMinimized);
			Win.Property("isMaximized", WinIsMaximized);
			Win.Property("zOrder", WinZOrder);
		EndIf;
		
		If Not ValueIsFilled(Title) Then
			Title = "(no title)";
		EndIf;
		
		// Build indented info string showing window hierarchy
		Indent = "";
		LvlIdx = 0;
		While LvlIdx < WinLevel Do
			Indent = Indent + "  ";
			LvlIdx = LvlIdx + 1;
		EndDo;
		
		Info = Indent + Title + " (" + String(WinWidth) + "x" + String(WinHeight) + ")";
		If IsMainWin = True Then
			Info = Info + " [MAIN]";
		EndIf;
		If IsModal = True Then
			Info = Info + " [MODAL]";
		EndIf;
		If WinIsMinimized = True Then
			Info = Info + " [MIN]";
		EndIf;
		If WinIsMaximized = True Then
			Info = Info + " [MAX]";
		EndIf;
		If WinIsEnabled <> True Then
			Info = Info + " [DISABLED]";
		EndIf;
		Info = Info + " [z=" + String(WinZOrder) + "]";
		
		PicAddress = "";
		If ValueIsFilled(ImageData) Then
			BinData = Base64Value(ImageData);
			PicAddress = PutToTempStorage(BinData, ThisObject.UUID);
		EndIf;
		
		Item = New Structure("Title,PictureAddress,Level,OwnerIndex",
			Info, PicAddress, WinLevel, WinOwnerIndex);
		ScreenshotDataArray.Add(Item);
	EndDo;
	
	If ScreenshotDataArray.Count() > 0 Then
		ScreenshotCurrentIndex = 0;
		ShowScreenshotByIndex(ScreenshotCurrentIndex);
	EndIf;
	
EndProcedure

&AtClient
Procedure ShowScreenshotByIndex(Index)
	
	If ScreenshotDataArray = Undefined Or ScreenshotDataArray.Count() = 0 Then
		ScreenshotInfo = "No screenshots";
		ScreenshotPicture = "";
		Return;
	EndIf;
	
	Item = ScreenshotDataArray[Index];
	ScreenshotInfo = "[" + String(Index + 1) + "/" + String(ScreenshotDataArray.Count()) + "] " + Item.Title;
	ScreenshotPicture = Item.PictureAddress;
	
EndProcedure

&AtClient
Procedure PrevScreenshot(Command)
	
	If ScreenshotDataArray = Undefined Or ScreenshotDataArray.Count() = 0 Then
		Return;
	EndIf;
	
	If ScreenshotCurrentIndex > 0 Then
		ScreenshotCurrentIndex = ScreenshotCurrentIndex - 1;
	Else
		ScreenshotCurrentIndex = ScreenshotDataArray.Count() - 1;
	EndIf;
	
	ShowScreenshotByIndex(ScreenshotCurrentIndex);
	
EndProcedure

&AtClient
Procedure NextScreenshot(Command)
	
	If ScreenshotDataArray = Undefined Or ScreenshotDataArray.Count() = 0 Then
		Return;
	EndIf;
	
	If ScreenshotCurrentIndex < ScreenshotDataArray.Count() - 1 Then
		ScreenshotCurrentIndex = ScreenshotCurrentIndex + 1;
	Else
		ScreenshotCurrentIndex = 0;
	EndIf;
	
	ShowScreenshotByIndex(ScreenshotCurrentIndex);
	
EndProcedure

#EndRegion


#Region ComponentLifecycle

&AtClient
Procedure InstallAddInEnd(AdditionalParameters) Export
	
	BeginAttachingAddIn(
		New NotifyDescription("AttachAddInEnd", ThisObject),
		AddInPath,
		"http1c",
		AddInType.Native);
	
EndProcedure

&AtClient
Procedure AttachAddInEnd(Connected, AdditionalParameters) Export
	
	If Not Connected Then
		ShowMessageBox(, "AttachAddIn failed. Source: " + AddInPath);
		Return;
	EndIf;
	
	Try
		Component = New("AddIn.http1c.HttpServer");
	Except
		ShowMessageBox(, "Component creation failed: " + ErrorDescription());
		Return;
	EndTry;

	ResetRuntimeStatus();
	EnsureLoggingDefaults();
	
	// All configuration via synchronous property assignments.
	Try
		Component.LoggingEnabled = EnableLogging;
		Component.LogPath = LogPath;
		Component.Timeout = 120;
	Except
		ShowMessageBox(, "Configuration failed: " + ErrorDescription());
		Return;
	EndTry;
	
	// Register MCP primitives (synchronous property assignments).
	RegisterMCPTools();
	RegisterMCPResources();
	RegisterMCPPrompts();
	
	// Start listening (async — returns via callback).
	PortValue = Port;
	If PortValue = 0 Then
		PortValue = 8888;
	EndIf;
	
	Try
		Component.BeginCallingStartListen(
			New NotifyDescription("StartListenEnd", ThisObject),
			PortValue);
	Except
		ShowMessageBox(, "StartListen failed: " + ErrorDescription());
	EndTry;
	
EndProcedure

&AtClient
Procedure StartListenEnd(ResultCall, ParametersCall, AdditionalParameters) Export
	
	PortValue = Port;
	If PortValue = 0 Then
		PortValue = 8888;
	EndIf;
	ShowMessageBox(, "MCP server started on port " + Format(PortValue, "NG=0"));
	
EndProcedure

&AtClient
Procedure AddInDetachmentOnError(Location, Name)
	
	Component = Undefined;
	MarkRuntimeFailure("Component detached: " + Name + " (" + Location + ")");
	ShowMessageBox(, RuntimeStatus.LastError);
	
EndProcedure

&AtClient
Procedure AttachAddInSSL(TemplateName, SymbolicName, NotifyDescription)
	
	BeginAttachingAddIn(NotifyDescription, TemplateName, SymbolicName, AddInType.Native);
	
EndProcedure

#EndRegion


// ============================================================================
// TOOL DEFINITIONS
// ============================================================================
//
// Each tool is defined as a JSON structure:
//   {
//     "name": "toolName",
//     "description": "What this tool does",
//     "inputSchema": { ... JSON Schema ... },
//     "annotations": { ... optional MCP annotations ... }
//   }
//
// Annotations (optional, per MCP spec):
//   "readOnlyHint"    : true/false - tool does not modify state
//   "destructiveHint" : true/false - tool may irreversibly modify state
//   "idempotentHint"  : true/false - calling multiple times has same effect
//   "openWorldHint"   : true/false - tool interacts with external entities
//
// To add a new tool:
//   1. Create a ToolXxx() function that returns the tool definition
//   2. Add it to the Tools array in RegisterMCPTools()
//   3. Add a handler in ProcessToolCall() dispatcher
//   4. Implement HandleXxx() procedure with Begin* callbacks
// ============================================================================

#Region ToolDefinitions

&AtClient
Procedure RegisterMCPTools()
	
	Tools = New Array;
	Tools.Add(ToolGetStatus());
	Tools.Add(ToolOpenForm());
	Tools.Add(ToolExecute());
	Tools.Add(ToolEvaluate());
	Tools.Add(ToolRunLongTask());
	Tools.Add(ToolTakeScreenshot());
	Tools.Add(ToolTestScreenshot());
	
	Component.Tools = SerializeToJson(Tools);
	
EndProcedure

&AtClient
Function NewTool(Name, Description)
	
	Tool = New Structure;
	Tool.Insert("name", Name);
	Tool.Insert("description", Description);
	Tool.Insert("inputSchema", NewObjectSchema());
	Return Tool;
	
EndFunction

&AtClient
Function NewObjectSchema()
	
	Schema = New Structure;
	Schema.Insert("type", "object");
	Schema.Insert("properties", New Structure);
	Schema.Insert("required", New Array);
	Return Schema;
	
EndFunction

&AtClient
Procedure AddToolParam(Tool, ParamName, ParamType, Description, IsRequired = True)
	
	PropertyDescription = New Structure("type,description", ParamType, Description);
	Tool.inputSchema.properties.Insert(ParamName, PropertyDescription);
	
	If IsRequired Then
		Tool.inputSchema.required.Add(ParamName);
	EndIf;
	
EndProcedure

// Add MCP annotations to a tool definition.
// Annotations help MCP clients understand tool behavior.
//
// Parameters:
//   Tool          - tool structure from NewTool()
//   ReadOnly      - true if the tool does not modify any state
//   Destructive   - true if the tool may irreversibly change data
//   Idempotent    - true if repeated calls produce the same result
//   OpenWorld     - true if the tool contacts external systems
&AtClient
Procedure AddToolAnnotations(Tool, ReadOnly = False, Destructive = False, Idempotent = False, OpenWorld = False)
	
	Annotations = New Structure;
	Annotations.Insert("readOnlyHint", ReadOnly);
	Annotations.Insert("destructiveHint", Destructive);
	Annotations.Insert("idempotentHint", Idempotent);
	Annotations.Insert("openWorldHint", OpenWorld);
	Tool.Insert("annotations", Annotations);
	
EndProcedure

// Build a JSON Schema object for outputSchema.
// Use AddOutputProperty() to add properties, then attach to tool with SetToolOutputSchema().
//
// Example:
//   Schema = NewOutputSchema();
//   AddOutputProperty(Schema, "temperature", "number", "Temperature in Celsius", True);
//   AddOutputProperty(Schema, "status", "string", "Server status text", True);
//   SetToolOutputSchema(Tool, Schema);
&AtClient
Function NewOutputSchema()
	
	Schema = New Structure;
	Schema.Insert("type", "object");
	Schema.Insert("properties", New Structure);
	Schema.Insert("required", New Array);
	Return Schema;
	
EndFunction

// Add a property to an outputSchema.
&AtClient
Procedure AddOutputProperty(Schema, PropName, PropType, Description, IsRequired = True)
	
	PropDef = New Structure("type,description", PropType, Description);
	Schema.properties.Insert(PropName, PropDef);
	
	If IsRequired Then
		Schema.required.Add(PropName);
	EndIf;
	
EndProcedure

// Attach an outputSchema to a tool definition.
// This tells MCP clients the expected structure of the tool's response.
&AtClient
Procedure SetToolOutputSchema(Tool, Schema)
	
	Tool.Insert("outputSchema", Schema);
	
EndProcedure

&AtClient
Function ToolGetStatus()
	
	Tool = NewTool("getStatus",
		"Return the current native component status, logging configuration, and the current 1C runtime state.");
	AddToolAnnotations(Tool, True);  // read-only, safe
	
	// outputSchema — structured result clients can validate
	Schema = NewOutputSchema();
	
	RuntimeProps = New Structure;
	RuntimeProps.Insert("type", "object");
	RuntimeDescription = New Structure;
	RuntimeDescription.Insert("type", "boolean");
	RuntimeDescription.Insert("description", "Whether the runtime is busy processing a request");
	RuntimeProps.Insert("properties", New Structure("IsBusy", RuntimeDescription));
	
	ComponentProps = New Structure;
	ComponentProps.Insert("type", "object");
	ComponentDescription = New Structure;
	ComponentDescription.Insert("type", "boolean");
	ComponentDescription.Insert("description", "Whether the HTTP server is running");
	ComponentProps.Insert("properties", New Structure("running", ComponentDescription));
	
	AddOutputProperty(Schema, "runtimeStatus", "object", "1C runtime state", True);
	AddOutputProperty(Schema, "componentStatus", "object", "Native component state", True);
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolOpenForm()
	
	Tool = NewTool("openForm",
		"Open a 1C:Enterprise form by its full name and return a confirmation message or an error.");
	AddToolParam(Tool, "formPath", "string",
		"Full form path, for example Catalog.Products.ListForm or ExternalDataProcessor.http1c.Form.Form.");
	AddToolParam(Tool, "parameters", "string",
		"Optional JSON string with OpenForm parameters.", False);
	AddToolAnnotations(Tool, False, False, True);  // not read-only, not destructive, idempotent
	
	// outputSchema — confirmation message
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "message", "string", "Confirmation message or error description");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolExecute()
	
	Tool = NewTool("execute",
		"Execute arbitrary 1C:Enterprise code on the server. The code must assign a string value to the Result variable.");
	AddToolParam(Tool, "code", "string",
		"Server-side 1C code. Example: Result = String(CurrentDate());");
	AddToolAnnotations(Tool, False, True);  // potentially destructive
	
	// outputSchema — the execution result
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "result", "string", "Value of the Result variable after execution");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolEvaluate()
	
	Tool = NewTool("evaluate",
		"Evaluate a 1C:Enterprise expression on the server and return its string representation.");
	AddToolParam(Tool, "expression", "string",
		"Expression text, for example CurrentDate() or Metadata.Documents.Count().");
	AddToolAnnotations(Tool, True, False, True);  // read-only, idempotent
	
	// outputSchema — the evaluation result
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "result", "string", "String representation of the evaluated expression");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolRunLongTask()
	
	Tool = NewTool("runLongTask",
		"Run a long test operation and report progress using MCP notifications/progress.");
	AddToolParam(Tool, "steps", "number",
		"Number of progress steps to emit.", False);
	AddToolParam(Tool, "iterationsPerStep", "number",
		"CPU work units performed on the server for each step.", False);
	AddToolAnnotations(Tool, True, False, False);  // read-only (test/benchmark)
	
	// outputSchema — task completion summary
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "completedSteps", "number", "Total number of steps completed");
	AddOutputProperty(Schema, "summary", "string", "Summary of the completed task");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolTakeScreenshot()
	
	Tool = NewTool("takeScreenshot",
		"Capture screenshots of all visible windows of the 1C:Enterprise process, including modal dialogs. Returns base64-encoded JPEG images by default (smaller size, optimal for AI). Supports PNG for lossless quality. Use grayscale=true to reduce image size when color is not needed.");
	AddToolParam(Tool, "pid", "number",
		"Process ID of the target 1C:Enterprise instance. Use 0 or omit to capture the current process.", False);
	AddToolParam(Tool, "format", "string",
		"Image format: 'jpeg' (default, smaller size) or 'png' (lossless).", False);
	AddToolParam(Tool, "quality", "number",
		"JPEG compression quality 1-100 (default 80). Lower values = smaller files. Ignored for PNG.", False);
	AddToolParam(Tool, "grayscale", "boolean",
		"Convert to grayscale (default false). Reduces file size; useful when color is not needed by the AI.", False);
	AddToolAnnotations(Tool, True);  // read-only, safe
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolTestScreenshot()
	
	Tool = NewTool("testScreenshot",
		"Test tool for capturing screenshots and returning them as MCP image content. Takes 3 required parameters.");
	AddToolParam(Tool, "pid", "number",
		"Process ID of the target process. Use 0 for the current 1C process.");
	AddToolParam(Tool, "format", "string",
		"Image format: 'jpeg' or 'png'.");
	AddToolParam(Tool, "quality", "number",
		"Compression quality 1-100 (for JPEG). Ignored for PNG.");
	AddToolParam(Tool, "grayscale", "boolean",
		"Convert to grayscale. Reduces file size when color is not needed.");
	AddToolAnnotations(Tool, True);  // read-only, safe
	
	Return Tool;
	
EndFunction

#EndRegion


#Region ToolDispatcher

&AtClient
Procedure ProcessToolCall(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	ToolName = Request["tool"];
	Arguments = Request["arguments"];
	
	If Arguments = Undefined Then
		Arguments = New Structure;
	EndIf;
	
	Try
		If ToolName = "getStatus" Then
			HandleGetStatus(RequestID);
		ElsIf ToolName = "openForm" Then
			HandleOpenForm(RequestID, Arguments);
		ElsIf ToolName = "execute" Then
			HandleExecute(RequestID, Arguments);
		ElsIf ToolName = "evaluate" Then
			HandleEvaluate(RequestID, Arguments);
		ElsIf ToolName = "runLongTask" Then
			HandleRunLongTask(RequestID, Arguments);
		ElsIf ToolName = "takeScreenshot" Then
			HandleTakeScreenshot(RequestID, Arguments);
		ElsIf ToolName = "testScreenshot" Then
			HandleTestScreenshot(RequestID, Arguments);
		Else
			SendToolError(RequestID, "Unknown tool: " + ToolName);
		EndIf;
	Except
		MarkRuntimeFailure("Dispatcher error for tool '" + ToolName + "': " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

#EndRegion


// ============================================================================
// RESOURCE DEFINITIONS
// ============================================================================
//
// MCP Resources provide contextual data to AI clients. Unlike tools, resources
// are read-only data that can be listed and fetched.
//
// Each resource is defined as:
//   {
//     "uri": "1c://metadata/catalogs",
//     "name": "1C Catalogs List",
//     "description": "List of all catalog metadata objects",
//     "mimeType": "application/json"
//   }
//
// When a client calls resources/read with a URI, the native component sends
// a "ResourceRead" ExternalEvent to 1C. The handler should respond with
// the resource content via SendResponse().
//
// To add a new resource:
//   1. Add a resource definition to RegisterMCPResources()
//   2. Add a handler in ProcessResourceRead() dispatcher
// ============================================================================

#Region ResourceDefinitions

&AtClient
Procedure RegisterMCPResources()
	
	Resources = New Array;
	
	// Example: expose 1C metadata catalog list as a resource
	Resource = New Structure;
	Resource.Insert("uri", "1c://metadata/catalogs");
	Resource.Insert("name", "1C Catalogs Metadata");
	Resource.Insert("description",
		"JSON list of all catalog (directory) metadata objects in the current 1C infobase, including their names and synonyms.");
	Resource.Insert("mimeType", "application/json");
	Resources.Add(Resource);
	
	// Example: expose 1C metadata document list as a resource
	Resource = New Structure;
	Resource.Insert("uri", "1c://metadata/documents");
	Resource.Insert("name", "1C Documents Metadata");
	Resource.Insert("description",
		"JSON list of all document metadata objects in the current 1C infobase.");
	Resource.Insert("mimeType", "application/json");
	Resources.Add(Resource);
	
	Component.Resources = SerializeToJson(Resources);
	
EndProcedure

#EndRegion


// ============================================================================
// PROMPT DEFINITIONS
// ============================================================================
//
// MCP Prompts are reusable interaction templates. They help AI applications
// structure their interactions with the 1C system.
//
// Each prompt is defined as:
//   {
//     "name": "promptName",
//     "description": "What this prompt template does",
//     "arguments": [
//       { "name": "argName", "description": "...", "required": true }
//     ]
//   }
//
// When a client calls prompts/get, the native component sends a "PromptGet"
// ExternalEvent. The handler returns a messages array per MCP spec.
//
// To add a new prompt:
//   1. Add a prompt definition to RegisterMCPPrompts()
//   2. Add a handler in ProcessPromptGet() dispatcher
// ============================================================================

#Region PromptDefinitions

&AtClient
Procedure RegisterMCPPrompts()
	
	Prompts = New Array;
	
	// Example: a prompt template for analyzing 1C data
	Prompt = New Structure;
	Prompt.Insert("name", "analyze1CData");
	Prompt.Insert("description",
		"Generate a system prompt for analyzing data in a 1C:Enterprise infobase. "
		+ "Provides context about available metadata and suggests analysis approaches.");
	PromptArgs = New Array;
	Arg = New Structure("name,description,required", "topic",
		"Analysis topic or area of interest (e.g., sales, inventory, HR).", False);
	PromptArgs.Add(Arg);
	Prompt.Insert("arguments", PromptArgs);
	Prompts.Add(Prompt);
	
	// Example: a prompt for generating 1C code
	Prompt = New Structure;
	Prompt.Insert("name", "generate1CCode");
	Prompt.Insert("description",
		"Generate a system prompt optimized for writing 1C:Enterprise BSL code. "
		+ "Includes coding conventions, common patterns, and available API references.");
	PromptArgs = New Array;
	Arg = New Structure("name,description,required", "task",
		"Description of the coding task.", True);
	PromptArgs.Add(Arg);
	Prompt.Insert("arguments", PromptArgs);
	Prompts.Add(Prompt);
	
	Component.Prompts = SerializeToJson(Prompts);
	
EndProcedure

#EndRegion


// ============================================================================
// RESOURCE DISPATCHER
// ============================================================================

#Region ResourceDispatcher

&AtClient
Procedure ProcessResourceRead(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	URI = Request["uri"];
	
	Try
		If URI = "1c://metadata/catalogs" Then
			HandleReadCatalogs(RequestID, URI);
		ElsIf URI = "1c://metadata/documents" Then
			HandleReadDocuments(RequestID, URI);
		Else
			SendResourceError(RequestID, "Unknown resource URI: " + URI);
		EndIf;
	Except
		SendResourceError(RequestID, "Resource read error: " + ErrorDescription());
	EndTry;
	
EndProcedure

#EndRegion


// ============================================================================
// PROMPT DISPATCHER
// ============================================================================

#Region PromptDispatcher

&AtClient
Procedure ProcessPromptGet(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	PromptName = Request["name"];
	Arguments = Request["arguments"];
	
	If Arguments = Undefined Then
		Arguments = New Structure;
	EndIf;
	
	Try
		If PromptName = "analyze1CData" Then
			HandlePromptAnalyze(RequestID, Arguments);
		ElsIf PromptName = "generate1CCode" Then
			HandlePromptGenerateCode(RequestID, Arguments);
		Else
			SendPromptError(RequestID, "Unknown prompt: " + PromptName);
		EndIf;
	Except
		SendPromptError(RequestID, "Prompt error: " + ErrorDescription());
	EndTry;
	
EndProcedure

#EndRegion


#Region ToolHandlers

&AtClient
Procedure HandleGetStatus(RequestID)
	
	Try
		Context = New Structure("RequestID", RequestID);
		BuildCombinedStatusJSON(
			New NotifyDescription("HandleGetStatusEnd", ThisObject, Context));
	Except
		MarkRuntimeFailure("Status retrieval failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleGetStatusEnd(StatusJson, AdditionalParameters) Export
	
	SendToolResult(AdditionalParameters.RequestID, StatusJson);
	
EndProcedure

&AtClient
Procedure HandleOpenForm(RequestID, Arguments)
	
	FormPath = GetArg(Arguments, "formPath");
	If Not ValueIsFilled(FormPath) Then
		SendToolError(RequestID, "Parameter 'formPath' is required.");
		Return;
	EndIf;
	
	MarkRuntimeStart(RequestID, "openForm", 1);
	SendToolProgress(RequestID, 0, 1, "Preparing to open the form.");
	
	Try
		ParametersJson = GetArg(Arguments, "parameters");
		FormParameters = ParseJsonArgument(ParametersJson, New Structure);
		OpenForm(FormPath, FormParameters);
		MarkRuntimeProgress(1, 1, "The form was opened successfully.");
		SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
		MarkRuntimeSuccess("Form opened: " + FormPath);
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Failed to open form '" + FormPath + "': " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleExecute(RequestID, Arguments)
	
	Code = GetArg(Arguments, "code");
	If Not ValueIsFilled(Code) Then
		SendToolError(RequestID, "Parameter 'code' is required.");
		Return;
	EndIf;
	
	MarkRuntimeStart(RequestID, "execute", 1);
	SendToolProgress(RequestID, 0, 1, "Executing server code.");
	
	Try
		ExecutionResult = ExecuteCodeOnServer(Code);
		MarkRuntimeProgress(1, 1, "Server code execution completed.");
		SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
		MarkRuntimeSuccess(ExecutionResult);
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Execution error: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtServer
Function ExecuteCodeOnServer(Val Code)
	
	Result = "";
	Execute(Code);
	Return String(Result);
	
EndFunction

&AtClient
Procedure HandleEvaluate(RequestID, Arguments)
	
	Expression = GetArg(Arguments, "expression");
	If Not ValueIsFilled(Expression) Then
		SendToolError(RequestID, "Parameter 'expression' is required.");
		Return;
	EndIf;
	
	MarkRuntimeStart(RequestID, "evaluate", 1);
	SendToolProgress(RequestID, 0, 1, "Evaluating expression.");
	
	Try
		EvaluationResult = EvaluateOnServer(Expression);
		MarkRuntimeProgress(1, 1, "Expression evaluation completed.");
		SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
		MarkRuntimeSuccess(EvaluationResult);
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Evaluation error: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtServer
Function EvaluateOnServer(Val Expression)
	
	Result = Eval(Expression);
	Return String(Result);
	
EndFunction

&AtClient
Procedure HandleRunLongTask(RequestID, Arguments)
	
	Steps = Max(1, NumberOrDefault(GetArg(Arguments, "steps"), 5));
	IterationsPerStep = Max(1, NumberOrDefault(GetArg(Arguments, "iterationsPerStep"), 4000000));
	
	MarkRuntimeStart(RequestID, "runLongTask", Steps);
	SendToolProgress(RequestID, 0, Steps, "Long-running test task started.");
	
	Try
		For StepIndex = 1 To Steps Do
			WorkSummary = RunLongTaskStepOnServer(IterationsPerStep, StepIndex, Steps);
			MarkRuntimeProgress(StepIndex, Steps, WorkSummary);
			SendToolProgress(RequestID, StepIndex, Steps, RuntimeStatus.ProgressMessage);
		EndDo;
		
		MarkRuntimeSuccess("Long-running test task completed.");
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Long-running task failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtServer
Function RunLongTaskStepOnServer(Val IterationsPerStep, Val StepIndex, Val TotalSteps)
	
	Accumulator = 0;
	For Counter = 1 To IterationsPerStep Do
		Accumulator = Accumulator + (Counter % 97);
	EndDo;
	
	Return "Completed step " + String(StepIndex) + " of " + String(TotalSteps)
		+ ". Accumulator=" + String(Accumulator);
	
EndFunction

&AtClient
Procedure HandleTakeScreenshot(RequestID, Arguments)
	
	PID = NumberOrDefault(GetArg(Arguments, "pid"), 0);
	
	Format = "jpeg";
	ArgFormat = GetArg(Arguments, "format");
	If ValueIsFilled(ArgFormat) Then
		Format = ArgFormat;
	EndIf;
	
	Quality = NumberOrDefault(GetArg(Arguments, "quality"), 80);
	
	Grayscale = False;
	ArgGrayscale = GetArg(Arguments, "grayscale");
	If ArgGrayscale = True Then
		Grayscale = True;
	EndIf;
	
	MarkRuntimeStart(RequestID, "takeScreenshot", 1);
	SendToolProgress(RequestID, 0, 1, "Capturing screenshots...");
	
	Try
		Context = New Structure("RequestID", RequestID);
		Component.BeginCallingTakeScreenshot(
			New NotifyDescription("HandleTakeScreenshotEnd", ThisObject, Context),
			PID, Format, Quality, Grayscale);
	Except
		MarkRuntimeFailure("Screenshot capture failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleTakeScreenshotEnd(ResultJson, ParametersCall, AdditionalParameters) Export
	
	RequestID = AdditionalParameters.RequestID;
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(ResultJson);
		CaptureResult = ReadJSON(JSONReader, True);
	Except
		MarkRuntimeFailure("Failed to parse screenshot result: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
		Return;
	EndTry;
	
	CaptureError = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		CaptureError = CaptureResult["error"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("error", CaptureError);
	EndIf;
	
	Windows = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		Windows = CaptureResult["windows"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("windows", Windows);
	EndIf;
	
	If Windows = Undefined Or Windows.Count() = 0 Then
		If Not ValueIsFilled(CaptureError) Then
			CaptureError = "No windows captured";
		EndIf;
		MarkRuntimeFailure(CaptureError);
		SendToolError(RequestID, CaptureError);
		Return;
	EndIf;
	
	// Build MCP content array with images and descriptions.
	Content = New Array;
	
	PID = 0;
	If TypeOf(CaptureResult) = Type("Map") Then
		PID = CaptureResult["pid"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("pid", PID);
	EndIf;
	
	SummaryItem = New Structure("type,text", "text",
		"Captured " + String(Windows.Count()) + " window(s) for PID " + String(PID));
	Content.Add(SummaryItem);
	
	For Each Win In Windows Do
		Title = "";
		If TypeOf(Win) = Type("Map") Then
			Title = Win["title"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("title", Title);
		EndIf;
		If Not ValueIsFilled(Title) Then
			Title = "(no title)";
		EndIf;
		
		WinWidth = 0;
		WinHeight = 0;
		IsModal = False;
		IsMainWin = False;
		ImageData = "";
		MimeType = "image/jpeg";
		WinError = "";
		WinLevel = 0;
		WinOwnerIndex = -1;
		WinZOrder = 0;
		WinIsEnabled = True;
		WinIsMinimized = False;
		WinIsMaximized = False;
		
		If TypeOf(Win) = Type("Map") Then
			WinWidth = Win["width"];
			WinHeight = Win["height"];
			IsModal = Win["isModal"];
			IsMainWin = Win["isMainWindow"];
			ImageData = Win["image"];
			MimeType = Win["mimeType"];
			WinError = Win["error"];
			WinLevel = Win["level"];
			WinOwnerIndex = Win["ownerIndex"];
			WinZOrder = Win["zOrder"];
			WinIsEnabled = Win["isEnabled"];
			WinIsMinimized = Win["isMinimized"];
			WinIsMaximized = Win["isMaximized"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("width", WinWidth);
			Win.Property("height", WinHeight);
			Win.Property("isModal", IsModal);
			Win.Property("isMainWindow", IsMainWin);
			Win.Property("image", ImageData);
			Win.Property("mimeType", MimeType);
			Win.Property("error", WinError);
			Win.Property("level", WinLevel);
			Win.Property("ownerIndex", WinOwnerIndex);
			Win.Property("zOrder", WinZOrder);
			Win.Property("isEnabled", WinIsEnabled);
			Win.Property("isMinimized", WinIsMinimized);
			Win.Property("isMaximized", WinIsMaximized);
		EndIf;
		
		// Build indented info string for AI (shows hierarchy via level)
		AIIndent = "";
		AILvlIdx = 0;
		While AILvlIdx < WinLevel Do
			AIIndent = AIIndent + "  ";
			AILvlIdx = AILvlIdx + 1;
		EndDo;
		Info = AIIndent + Title + " (" + String(WinWidth) + "x" + String(WinHeight) + ")";
		If IsMainWin = True Then
			Info = Info + " [MAIN]";
		EndIf;
		If IsModal = True Then
			Info = Info + " [MODAL]";
		EndIf;
		If WinIsMinimized = True Then
			Info = Info + " [MIN]";
		EndIf;
		If WinIsMaximized = True Then
			Info = Info + " [MAX]";
		EndIf;
		If WinIsEnabled <> True Then
			Info = Info + " [DISABLED]";
		EndIf;
		Info = Info + " [z=" + String(WinZOrder) + "]";
		If WinOwnerIndex >= 0 Then
			Info = Info + " [owner=#" + String(WinOwnerIndex) + "]";
		EndIf;
		
		TextItem = New Structure("type,text", "text", Info);
		Content.Add(TextItem);
		
		If ValueIsFilled(ImageData) Then
			ImageItem = New Structure("type,data,mimeType", "image", ImageData, MimeType);
			Content.Add(ImageItem);
		Else
			If Not ValueIsFilled(WinError) Then
				WinError = "Capture failed";
			EndIf;
			ErrItem = New Structure("type,text", "text", "Error: " + WinError);
			Content.Add(ErrItem);
		EndIf;
	EndDo;
	
	MarkRuntimeProgress(1, 1, "Screenshot captured: " + String(Windows.Count()) + " window(s)");
	SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
	MarkRuntimeSuccess("Screenshot captured: " + String(Windows.Count()) + " window(s)");
	
	// Send custom content with images (bypassing SendToolResult which only creates text).
	ToolResult = New Structure;
	ToolResult.Insert("content", Content);
	SendMCPResponse(RequestID, ToolResult);
	
EndProcedure

&AtClient
Procedure HandleTestScreenshot(RequestID, Arguments)
	
	PID = NumberOrDefault(GetArg(Arguments, "pid"), 0);
	
	Format = "jpeg";
	ArgFormat = GetArg(Arguments, "format");
	If ValueIsFilled(ArgFormat) Then
		Format = ArgFormat;
	EndIf;
	
	Quality = NumberOrDefault(GetArg(Arguments, "quality"), 80);
	
	Grayscale = False;
	ArgGrayscale = GetArg(Arguments, "grayscale");
	If ArgGrayscale = True Then
		Grayscale = True;
	EndIf;
	
	MarkRuntimeStart(RequestID, "testScreenshot", 1);
	SendToolProgress(RequestID, 0, 1, "Test: capturing screenshots...");
	
	Try
		Context = New Structure("RequestID", RequestID);
		Component.BeginCallingTakeScreenshot(
			New NotifyDescription("HandleTestScreenshotEnd", ThisObject, Context),
			PID, Format, Quality, Grayscale);
	Except
		MarkRuntimeFailure("Test screenshot failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleTestScreenshotEnd(ResultJson, ParametersCall, AdditionalParameters) Export
	
	RequestID = AdditionalParameters.RequestID;
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(ResultJson);
		CaptureResult = ReadJSON(JSONReader, True);
	Except
		MarkRuntimeFailure("Failed to parse test screenshot result: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
		Return;
	EndTry;
	
	CaptureError = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		CaptureError = CaptureResult["error"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("error", CaptureError);
	EndIf;
	
	Windows = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		Windows = CaptureResult["windows"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("windows", Windows);
	EndIf;
	
	If Windows = Undefined Or Windows.Count() = 0 Then
		If Not ValueIsFilled(CaptureError) Then
			CaptureError = "No windows captured";
		EndIf;
		MarkRuntimeFailure(CaptureError);
		SendToolError(RequestID, CaptureError);
		Return;
	EndIf;
	
	Content = New Array;
	
	PID = 0;
	If TypeOf(CaptureResult) = Type("Map") Then
		PID = CaptureResult["pid"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("pid", PID);
	EndIf;
	
	SummaryItem = New Structure("type,text", "text",
		"[TEST] Captured " + String(Windows.Count()) + " window(s) for PID " + String(PID));
	Content.Add(SummaryItem);
	
	For Each Win In Windows Do
		Title = "";
		MimeType = "image/jpeg";
		ImageData = "";
		IsModal = False;
		IsMainWin = False;
		WinWidth = 0;
		WinHeight = 0;
		WinError = "";
		
		If TypeOf(Win) = Type("Map") Then
			Title = Win["title"];
			MimeType = Win["mimeType"];
			ImageData = Win["image"];
			IsModal = Win["isModal"];
			IsMainWin = Win["isMainWindow"];
			WinWidth = Win["width"];
			WinHeight = Win["height"];
			WinError = Win["error"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("title", Title);
			Win.Property("mimeType", MimeType);
			Win.Property("image", ImageData);
			Win.Property("isModal", IsModal);
			Win.Property("isMainWindow", IsMainWin);
			Win.Property("width", WinWidth);
			Win.Property("height", WinHeight);
			Win.Property("error", WinError);
		EndIf;
		
		If Not ValueIsFilled(Title) Then
			Title = "(no title)";
		EndIf;
		
		Info = Title + " (" + String(WinWidth) + "x" + String(WinHeight) + ")";
		If IsModal = True Then
			Info = Info + " [MODAL]";
		EndIf;
		If IsMainWin = True Then
			Info = Info + " [MAIN]";
		EndIf;
		
		TextItem = New Structure("type,text", "text", Info);
		Content.Add(TextItem);
		
		If ValueIsFilled(ImageData) Then
			ImageItem = New Structure("type,data,mimeType", "image", ImageData, MimeType);
			Content.Add(ImageItem);
		Else
			If Not ValueIsFilled(WinError) Then
				WinError = "Capture failed";
			EndIf;
			ErrItem = New Structure("type,text", "text", "Error: " + WinError);
			Content.Add(ErrItem);
		EndIf;
	EndDo;
	
	MarkRuntimeProgress(1, 1, "[TEST] Screenshot captured: " + String(Windows.Count()) + " window(s)");
	SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
	MarkRuntimeSuccess("[TEST] Screenshot captured: " + String(Windows.Count()) + " window(s)");
	
	ToolResult = New Structure;
	ToolResult.Insert("content", Content);
	SendMCPResponse(RequestID, ToolResult);
	
EndProcedure

#EndRegion
// ============================================================================
//
// Each handler reads data from the 1C infobase and sends it back in
// MCP resources/read response format:
//   {
//     "contents": [
//       { "uri": "1c://...", "mimeType": "application/json", "text": "..." }
//     ]
//   }
// ============================================================================

#Region ResourceHandlers

&AtClient
Procedure HandleReadCatalogs(RequestID, URI)
	
	CatalogData = GetCatalogMetadataOnServer();
	
	ResourceResult = New Structure;
	Contents = New Array;
	ContentItem = New Structure;
	ContentItem.Insert("uri", URI);
	ContentItem.Insert("mimeType", "application/json");
	ContentItem.Insert("text", CatalogData);
	Contents.Add(ContentItem);
	ResourceResult.Insert("contents", Contents);
	
	SendResourceResult(RequestID, ResourceResult);
	
EndProcedure

&AtServer
Function GetCatalogMetadataOnServer()
	
	CatalogList = New Array;
	For Each Cat In Metadata.Catalogs Do
		Item = New Structure;
		Item.Insert("name", Cat.Name);
		Item.Insert("synonym", String(Cat.Synonym));
		Item.Insert("attributeCount", Cat.Attributes.Count());
		Item.Insert("tabularsCount", Cat.TabularSections.Count());
		CatalogList.Add(Item);
	EndDo;
	
	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, CatalogList);
	Return JSONWriter.Close();
	
EndFunction

&AtClient
Procedure HandleReadDocuments(RequestID, URI)
	
	DocumentData = GetDocumentMetadataOnServer();
	
	ResourceResult = New Structure;
	Contents = New Array;
	ContentItem = New Structure;
	ContentItem.Insert("uri", URI);
	ContentItem.Insert("mimeType", "application/json");
	ContentItem.Insert("text", DocumentData);
	Contents.Add(ContentItem);
	ResourceResult.Insert("contents", Contents);
	
	SendResourceResult(RequestID, ResourceResult);
	
EndProcedure

&AtServer
Function GetDocumentMetadataOnServer()
	
	DocumentList = New Array;
	For Each Doc In Metadata.Documents Do
		Item = New Structure;
		Item.Insert("name", Doc.Name);
		Item.Insert("synonym", String(Doc.Synonym));
		Item.Insert("attributeCount", Doc.Attributes.Count());
		Item.Insert("tabularsCount", Doc.TabularSections.Count());
		DocumentList.Add(Item);
	EndDo;
	
	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, DocumentList);
	Return JSONWriter.Close();
	
EndFunction

#EndRegion


// ============================================================================
// PROMPT HANDLERS
// ============================================================================
//
// Each handler builds an MCP messages array per the prompts/get response format:
//   {
//     "messages": [
//       { "role": "user", "content": { "type": "text", "text": "..." } }
//     ]
//   }
// ============================================================================

#Region PromptHandlers

&AtClient
Procedure HandlePromptAnalyze(RequestID, Arguments)
	
	Topic = GetArg(Arguments, "topic");
	If Not ValueIsFilled(Topic) Then
		Topic = "general overview";
	EndIf;
	
	MetadataSummary = GetMetadataSummaryOnServer();
	
	PromptText = "You are an expert 1C:Enterprise data analyst. "
		+ "Analyze the following data from a 1C infobase on the topic: " + Topic + "."
		+ Chars.LF + Chars.LF
		+ "Available metadata in the infobase:" + Chars.LF + MetadataSummary
		+ Chars.LF + Chars.LF
		+ "Use the 'evaluate' and 'execute' tools to query data and perform analysis. "
		+ "Present findings in a structured format with actionable insights.";
	
	Messages = New Array;
	Message = New Structure;
	Message.Insert("role", "user");
	Content = New Structure("type,text", "text", PromptText);
	Message.Insert("content", Content);
	Messages.Add(Message);
	
	PromptResult = New Structure("messages", Messages);
	SendPromptResult(RequestID, PromptResult);
	
EndProcedure

&AtServer
Function GetMetadataSummaryOnServer()
	
	Lines = New Array;
	Lines.Add("Catalogs: " + String(Metadata.Catalogs.Count()));
	Lines.Add("Documents: " + String(Metadata.Documents.Count()));
	Lines.Add("Information Registers: " + String(Metadata.InformationRegisters.Count()));
	Lines.Add("Accumulation Registers: " + String(Metadata.AccumulationRegisters.Count()));
	
	For Each Cat In Metadata.Catalogs Do
		Lines.Add("  - Catalog." + Cat.Name + " (" + String(Cat.Synonym) + ")");
	EndDo;
	For Each Doc In Metadata.Documents Do
		Lines.Add("  - Document." + Doc.Name + " (" + String(Doc.Synonym) + ")");
	EndDo;
	
	Result = "";
	For Each Line In Lines Do
		Result = Result + Line + Chars.LF;
	EndDo;
	Return Result;
	
EndFunction

&AtClient
Procedure HandlePromptGenerateCode(RequestID, Arguments)
	
	Task = GetArg(Arguments, "task");
	If Not ValueIsFilled(Task) Then
		SendPromptError(RequestID, "Parameter 'task' is required.");
		Return;
	EndIf;
	
	PromptText = "You are an expert 1C:Enterprise BSL developer. "
		+ "Write clean, well-documented 1C code for the following task: " + Task + "."
		+ Chars.LF + Chars.LF
		+ "Follow these coding conventions:"
		+ Chars.LF + "- Use meaningful Russian or English variable names"
		+ Chars.LF + "- Add comments explaining business logic"
		+ Chars.LF + "- Use Begin* callback pattern for component calls"
		+ Chars.LF + "- Handle errors with Try/Except"
		+ Chars.LF + "- Separate server and client code with proper directives"
		+ Chars.LF + Chars.LF
		+ "You can test code using the 'execute' tool by assigning the result to the Result variable. "
		+ "Use 'evaluate' for quick expression checks.";
	
	Messages = New Array;
	Message = New Structure;
	Message.Insert("role", "user");
	Content = New Structure("type,text", "text", PromptText);
	Message.Insert("content", Content);
	Messages.Add(Message);
	
	PromptResult = New Structure("messages", Messages);
	SendPromptResult(RequestID, PromptResult);
	
EndProcedure

#EndRegion


#Region RuntimeStatus

&AtClient
Procedure ResetRuntimeStatus()
	
	RuntimeStatus = New Structure;
	RuntimeStatus.Insert("IsBusy", False);
	RuntimeStatus.Insert("CurrentRequestID", "");
	RuntimeStatus.Insert("CurrentTool", "");
	RuntimeStatus.Insert("Progress", 0);
	RuntimeStatus.Insert("Total", 0);
	RuntimeStatus.Insert("ProgressMessage", "Idle");
	RuntimeStatus.Insert("LastResult", "");
	RuntimeStatus.Insert("LastError", "");
	RuntimeStatus.Insert("UpdatedAt", String(CurrentDate()));
	
EndProcedure

&AtClient
Procedure MarkRuntimeStart(RequestID, ToolName, Total)
	
	EnsureRuntimeStatus();
	RuntimeStatus.IsBusy = True;
	RuntimeStatus.CurrentRequestID = RequestID;
	RuntimeStatus.CurrentTool = ToolName;
	RuntimeStatus.Progress = 0;
	RuntimeStatus.Total = Total;
	RuntimeStatus.ProgressMessage = "Started.";
	RuntimeStatus.LastError = "";
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure MarkRuntimeProgress(Progress, Total, Message)
	
	EnsureRuntimeStatus();
	RuntimeStatus.Progress = Progress;
	RuntimeStatus.Total = Total;
	RuntimeStatus.ProgressMessage = Message;
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure MarkRuntimeSuccess(ResultText)
	
	EnsureRuntimeStatus();
	RuntimeStatus.IsBusy = False;
	RuntimeStatus.LastResult = ResultText;
	RuntimeStatus.LastError = "";
	RuntimeStatus.ProgressMessage = "Completed.";
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure MarkRuntimeFailure(ErrorText)
	
	EnsureRuntimeStatus();
	RuntimeStatus.IsBusy = False;
	RuntimeStatus.LastError = ErrorText;
	RuntimeStatus.ProgressMessage = "Failed.";
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure EnsureRuntimeStatus()
	
	If RuntimeStatus = Undefined Then
		ResetRuntimeStatus();
	EndIf;
	
EndProcedure

&AtClient
Procedure EnsureLoggingDefaults()
	
	If DefaultLogPath = Undefined Then
		DefaultLogPath = "http_debug.log";
	EndIf;
	
	If LogPath = Undefined Or Not ValueIsFilled(LogPath) Then
		LogPath = DefaultLogPath;
	EndIf;
	
	If EnableLogging = Undefined Then
		EnableLogging = True;
	EndIf;
	
EndProcedure

&AtClient
Procedure ApplyLoggingSettings()
	
	EnsureLoggingDefaults();
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	Try
		Component.LoggingEnabled = EnableLogging;
		Component.LogPath = LogPath;
	Except
		ShowMessageBox(, "ConfigureLogging failed: " + ErrorDescription());
	EndTry;
	
EndProcedure

&AtClient
Procedure BuildCombinedStatusJSON(Callback)
	
	EnsureRuntimeStatus();
	
	StatusPayload = New Structure;
	StatusPayload.Insert("runtimeStatus", RuntimeStatus);
	
	If Component <> Undefined Then
		Context = New Structure("Callback,StatusPayload", Callback, StatusPayload);
		Try
			StatusJson = Component.Status;
		Except
			StatusJson = "";
		EndTry;
		BuildCombinedStatusEnd(StatusJson, Context);
	Else
		StatusPayload.Insert("componentStatus", New Structure("running", False));
		ExecuteNotifyProcessing(Callback, SerializeToJson(StatusPayload));
	EndIf;
	
EndProcedure

&AtClient
Procedure BuildCombinedStatusEnd(ResultCall, AdditionalParameters) Export
	
	AdditionalParameters.StatusPayload.Insert("componentStatus",
		ParseJsonArgument(ResultCall, ResultCall));
	ExecuteNotifyProcessing(AdditionalParameters.Callback, SerializeToJson(AdditionalParameters.StatusPayload));
	
EndProcedure

&AtServer
Function GetDefaultAddInSource()
	
	Obj = FormAttributeToValue("Object");
	Tmp = Obj.GetTemplate("http1c");
	Addr = PutToTempStorage(Tmp, UUID);
	Return Addr;

EndFunction

&AtClient
Function ExtCompTRegistryDir()
	// Filesystem directory the launcher populates with registry.xml +
	// libhttp1cWin.dll + rcore.dll + DirectML.dll. Attaching a native component
	// from this path makes 1C load it in-place, so rcore.dll (loaded at runtime
	// by the component) is found beside it and real search is enabled. Mirrors
	// Vanessa Automation's silent registry.xml technique without any install modal.
	Return "D:\GitHub\MCP-DB-Client\rust-core\target\extcomp";
EndFunction

#EndRegion


// ============================================================================
// MCP TRANSPORT
// ============================================================================
//
// Low-level methods for sending responses back to the native component.
// These wrap the JSON-RPC response format required by MCP.
//
// For tools:
//   { "content": [{"type": "text", "text": "..."}], "isError": false }
// For resources:
//   { "contents": [{"uri": "...", "mimeType": "...", "text": "..."}] }
// For prompts:
//   { "messages": [{"role": "user", "content": {"type": "text", "text": "..."}}] }
// ============================================================================

#Region MCPTransport

// ---- Tool transport ----

&AtClient
Procedure SendToolResult(RequestID, Text)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, NewMCPContent(Text, False));
	
EndProcedure

&AtClient
Procedure SendToolError(RequestID, ErrorText)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, NewMCPContent(ErrorText, True));
	
EndProcedure

&AtClient
Procedure SendToolProgress(RequestID, Progress, Total, Message)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	Component.BeginCallingSendProgress(
		New NotifyDescription("EmptyCallbackHandler", ThisObject),
		RequestID, Progress, Total, Message);
	
EndProcedure

// ---- Resource transport ----

&AtClient
Procedure SendResourceResult(RequestID, ResourceResult)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, ResourceResult);
	
EndProcedure

&AtClient
Procedure SendResourceError(RequestID, ErrorText)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	ErrorResult = New Structure;
	ErrorResult.Insert("error", ErrorText);
	SendMCPResponse(RequestID, ErrorResult);
	
EndProcedure

// ---- Prompt transport ----

&AtClient
Procedure SendPromptResult(RequestID, PromptResult)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, PromptResult);
	
EndProcedure

&AtClient
Procedure SendPromptError(RequestID, ErrorText)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	ErrorResult = New Structure;
	ErrorResult.Insert("error", ErrorText);
	SendMCPResponse(RequestID, ErrorResult);
	
EndProcedure

// ---- Common ----

&AtClient
Procedure SendMCPResponse(RequestID, ResultStructure)
	
	Response = New Structure;
	Response.Insert("id", RequestID);
	Response.Insert("body", SerializeToJson(ResultStructure));
	
	Component.BeginCallingSendResponse(
		New NotifyDescription("EmptyCallbackHandler", ThisObject),
		SerializeToJson(Response));
	
EndProcedure

&AtClient
Function NewMCPContent(Text, IsError = False)
	
	ContentItem = New Structure;
	ContentItem.Insert("type", "text");
	ContentItem.Insert("text", Text);
	
	Content = New Array;
	Content.Add(ContentItem);
	
	Result = New Structure;
	Result.Insert("content", Content);
	
	If IsError Then
		Result.Insert("isError", True);
	EndIf;
	
	Return Result;
	
EndFunction

#EndRegion


#Region LegacyHTTP

&AtClient
Procedure ProcessLegacyRequest(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	SendHTTPResponse(RequestID, 404,
		New Structure("error", "Use POST /mcp for MCP requests."));
	
EndProcedure

&AtClient
Procedure SendHTTPResponse(ID, Status, ResponseData, ContentType = "application/json")
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	Response = New Structure;
	Response.Insert("id", ID);
	Response.Insert("status", Status);
	Response.Insert("content_type", ContentType);
	Response.Insert("body", SerializeToJson(ResponseData));
	
	Component.BeginCallingSendResponse(
		New NotifyDescription("EmptyCallbackHandler", ThisObject),
		SerializeToJson(Response));
	
EndProcedure

#EndRegion


// ============================================================================
// RAG / SEARCH DEMO
// ============================================================================
//
// Drives the Rust search core (rcore.dll) from 1C via the component's
// RagDispatch(method, payloadJson) method. Ingest/admin calls (configure,
// index_segments, stats, search, delete_collection) are issued from here; the
// search/grep/get_segment MCP tools are served by the component itself (no 1C
// round-trip), so once data is indexed an MCP client can search it too.
//
// End-to-end test flow:
//   1. Connect              - load the native component.
//   2. Configure RAG        - select the e5 model (needs the FULL package + a
//                             staged model; the lite component answers
//                             rag_not_installed).
//   3. Index demo data      - this base's catalog + document metadata as segments.
//   4. Stats                - watch vector_status go building -> ready.
//   5. Search               - dense / keyword / hybrid.
// ============================================================================

#Region RAGDemo

&AtClient
Procedure EnsureRagDefaults()

	If Port = 0 Then
		Port = 8888;
	EndIf;
	If Not ValueIsFilled(RagModel) Then
		// Offline MiniLM-L6 dir for fast tests (has a ":" so treated as model_path).
		// e5-small int8 stays available at ...\offline-model if multilingual quality
		// is needed.
		RagModel = "D:\GitHub\MCP-DB-Client\rust-core\target\offline-model-mini";
	EndIf;
	If Not ValueIsFilled(RagDevice) Then
		RagDevice = "auto";
	EndIf;
	If Not ValueIsFilled(RagCollection) Then
		RagCollection = "metadata";
	EndIf;
	If Not ValueIsFilled(RagMode) Then
		RagMode = "hybrid";
	EndIf;

EndProcedure

// ---- Command handlers ----

&AtClient
Procedure RagConfigure(Command)

	EnsureRagDefaults();

	Payload = New Structure;
	If RagModelLooksLikePath(RagModel) Then
		Payload.Insert("model_path", RagModel);
	Else
		Payload.Insert("model", RagModel);
	EndIf;
	Payload.Insert("device", RagDevice);

	RagCall("configure", SerializeToJson(Payload), "RagConfigureEnd");

EndProcedure

&AtClient
Procedure RagConfigureEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("configure", ResultJson);

EndProcedure

&AtClient
Procedure RagIndexDemo(Command)

	EnsureRagDefaults();

	PayloadJson = BuildMetadataSegmentsJSON(RagCollection);
	RagCall("index_segments", PayloadJson, "RagIndexDemoEnd");

EndProcedure

&AtClient
Procedure RagIndexDemoEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("index_segments", ResultJson);
	// Poll embedding progress and show a progress indicator in the form until the
	// background worker has embedded every segment (vector_status = ready).
	AttachIdleHandler("RagIndexPollTick", 1, True);

EndProcedure

&AtClient
Procedure RagIndexPollTick() Export

	RagCall("stats", "{}", "RagIndexPollEnd");

EndProcedure

&AtClient
Procedure RagIndexPollEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	Emb = 0;
	Total = 0;
	VecStatus = "";
	Try
		R = New JSONReader;
		R.SetString(ResultJson);
		Obj = ReadJSON(R, True);
		Coll = Obj["result"]["collections"][RagCollection];
		If Coll <> Undefined Then
			Emb = Coll["embedded"];
			Total = Coll["n_segments"];
			VecStatus = Coll["vector_status"];
		EndIf;
	Except
	EndTry;

	ShowEmbedProgress("Эмбеддинг векторов: " + RagCollection, Emb, Total);

	If VecStatus <> "ready" Then
		AttachIdleHandler("RagIndexPollTick", 1, True);
	Else
		Status(); // clear the progress indicator
		RagOutput = "Готово: эмбеддинг " + String(Emb) + " из " + String(Total) + " (100%)";
	EndIf;

EndProcedure

&AtClient
Procedure RagStats(Command)

	RagCall("stats", "{}", "RagStatsEnd");

EndProcedure

&AtClient
Procedure RagStatsEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("stats", ResultJson);

EndProcedure

&AtClient
Procedure RagSearch(Command)

	EnsureRagDefaults();

	If Not ValueIsFilled(RagQuery) Then
		ShowMessageBox(, "Enter a search query first.");
		Return;
	EndIf;

	Payload = New Structure;
	Payload.Insert("query", RagQuery);
	Payload.Insert("collection", RagCollection);
	Payload.Insert("mode", RagMode);
	Payload.Insert("k", 10);
	Payload.Insert("include_text", True);

	RagCall("search", SerializeToJson(Payload), "RagSearchEnd");

EndProcedure

&AtClient
Procedure RagSearchEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("search", ResultJson);

EndProcedure

&AtClient
Procedure RagClear(Command)

	EnsureRagDefaults();

	Payload = New Structure("collection", RagCollection);
	RagCall("delete_collection", SerializeToJson(Payload), "RagClearEnd");

EndProcedure

&AtClient
Procedure RagClearEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("delete_collection", ResultJson);

EndProcedure

// ---- Helpers ----

&AtClient
Function RagModelLooksLikePath(Value)

	Return StrFind(Value, "\") > 0 Or StrFind(Value, "/") > 0 Or StrFind(Value, ":") > 0;

EndFunction

&AtClient
Procedure RagCall(Method, PayloadJson, CallbackName)

	If Component = Undefined Then
		ShowMessageBox(, "Component is not connected. Press Connect first.");
		Return;
	EndIf;

	Try
		Component.BeginCallingRagDispatch(
			New NotifyDescription(CallbackName, ThisObject),
			Method, PayloadJson);
	Except
		ShowMessageBox(, "RagDispatch('" + Method + "') failed: " + ErrorDescription());
	EndTry;

EndProcedure

&AtClient
Procedure ShowEmbedProgress(Stage, Emb, Total)

	Pct = ?(Total > 0, Int(Emb * 100 / Total), 0);
	Descr = String(Emb) + " из " + String(Total) + " (" + String(Pct) + "%)";
	// Native 1C progress indicator: message + 0..100 bar + explanatory text.
	Status(Stage, Pct, Descr);
	// Also mirror into the visible output field on the form.
	RagOutput = Stage + ": " + Descr;

EndProcedure

&AtClient
Procedure ShowRagResult(Label, ResultJson)

	Envelope = ParseJsonArgument(ResultJson, Undefined);

	Lines = New Array;
	Lines.Add("=== " + Label + " ===");

	If Envelope = Undefined Then
		Lines.Add("(unparseable response)");
		Lines.Add(Left(ResultJson, 4000));
		RagOutput = JoinRagLines(Lines);
		Return;
	EndIf;

	If GetArg(Envelope, "ok") = True Then
		ResultValue = GetArg(Envelope, "result");
		Hits = Undefined;
		If TypeOf(ResultValue) = Type("Map") Then
			Hits = ResultValue["hits"];
		EndIf;

		If Hits <> Undefined Then
			Lines.Add("hits: " + String(Hits.Count()));
			For Each Hit In Hits Do
				DocId = GetArg(Hit, "doc_id");
				Score = GetArg(Hit, "score");
				Text = GetArg(Hit, "text");
				Snippet = "";
				If Text <> Undefined Then
					Snippet = Left(StrReplace(String(Text), Chars.LF, " "), 100);
				EndIf;
				ScoreStr = ?(Score = Undefined, "", Format(Score, "NFD=4; NG=0"));
				Lines.Add("  [" + ScoreStr + "] " + String(DocId) + " — " + Snippet);
			EndDo;
		Else
			Lines.Add("OK: " + SerializeToJson(ResultValue));
		EndIf;
	Else
		ErrorObj = GetArg(Envelope, "error");
		Lines.Add("ERROR [" + String(GetArg(ErrorObj, "code")) + "]: "
			+ String(GetArg(ErrorObj, "message")));
	EndIf;

	RagOutput = JoinRagLines(Lines);

EndProcedure

&AtClient
Function JoinRagLines(Lines)

	Result = "";
	For Each Line In Lines Do
		Result = Result + Line + Chars.LF;
	EndDo;
	Return Result;

EndFunction

&AtServer
Function BuildMetadataSegmentsJSON(Collection)

	// Index this base's catalog + document metadata as searchable segments
	// (a single doc_id "metadata" with many segments). Capped so a very large
	// configuration does not build an enormous payload for a demo.
	MaxSegments = 500;

	Segments = New Array;

	For Each Cat In Metadata.Catalogs Do
		If Segments.Count() >= MaxSegments Then
			Break;
		EndIf;
		Segments.Add(New Structure("text",
			"Справочник " + Cat.Name + " — " + String(Cat.Synonym)));
	EndDo;

	For Each Doc In Metadata.Documents Do
		If Segments.Count() >= MaxSegments Then
			Break;
		EndIf;
		Segments.Add(New Structure("text",
			"Документ " + Doc.Name + " — " + String(Doc.Synonym)));
	EndDo;

	Payload = New Structure;
	Payload.Insert("collection", Collection);
	Payload.Insert("doc_id", "metadata");
	Payload.Insert("name", "1C metadata (catalogs + documents)");
	Payload.Insert("segments", Segments);

	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, Payload);
	Return JSONWriter.Close();

EndFunction

#EndRegion


#Region RAGSelfTest

// Headless self-test driven by the /C"ragselftest" launch parameter (see OnOpen).
// Synchronously attaches the component and round-trips RagDispatch, writing the
// envelopes to rust-core/target/ragselftest-result.txt. The launcher reads that
// file and terminates the 1C process it itself started (by its own PID).

// Async full-chain self-test (this config forbids synchronous component calls).
// attach-only: the component is already cached in ExtCompT (the launcher stages
// libhttp1cWin.dll + rcore.dll + DirectML.dll there), so no InstallAddIn is
// needed and rcore.dll loaded beside the component enables real search.
// Flow: attach -> configure(local e5 model) -> index steps.json -> wait for
// embedding -> hybrid search. Each step is logged; the launcher waits for "DONE".

&AtClient
Procedure RunRagSelfTestDeferred()

	Ctx = New Structure;
	Ctx.Insert("Log", New Array);
	SelfTestAppend(Ctx, "STARTED " + String(CurrentDate()) + " build=" + BuildVersion());

	// The component was already attached on open from the on-disk registry.xml
	// directory (rcore.dll beside it), so go straight to configure — no install,
	// no re-attach, no modal.
	If Component = Undefined Then
		SelfTestAppend(Ctx, "Component not attached on open");
		SelfTestAppend(Ctx, "DONE");
		Return;
	EndIf;
	Try
		SelfTestAppend(Ctx, "version = " + Component.Version);
	Except
		SelfTestAppend(Ctx, "Version EXCEPTION: " + ErrorDescription());
	EndTry;

	// Point at the locally-staged offline e5 model (CPU) so rcore loads via
	// new_local with no network fetch. On the lite component this returns
	// rag_not_installed and the chain stops at SelfTest_ConfigureEnd.
	Cfg = New Structure;
	Cfg.Insert("model_path", "D:\GitHub\MCP-DB-Client\rust-core\target\offline-model-mini");
	// GPU (DirectML) with automatic CPU fallback (per user: will be tested on a
	// GPU box). On a CPU-only machine ort silently falls back to CPU.
	Cfg.Insert("device", "auto");
	Ctx.Insert("TCfg", Ms());
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_ConfigureEnd", ThisObject, Ctx),
		"configure", SerializeToJson(Cfg));

EndProcedure

&AtClient
Procedure SelfTest_ConfigureEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "configure (model load) took " + String(Ms() - Ctx.TCfg) + " ms");
	SelfTestAppend(Ctx, "configure -> " + Left(ResultJson, 300));
	If StrFind(ResultJson, """ok"":true") = 0 Then
		SelfTestAppend(Ctx, "DONE");
		Return;
	EndIf;

	StepsJson = BuildStepsSegmentsJSON("steps", 300);
	SelfTestAppend(Ctx, "indexing step segments (" + String(StrLen(StepsJson)) + " bytes)");
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_IndexEnd", ThisObject, Ctx),
		"index_segments", StepsJson);

EndProcedure

&AtClient
Procedure SelfTest_IndexEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "index_segments -> " + Left(ResultJson, 200));
	// Poll stats until the background embedder finishes (vector_status "ready"),
	// then search — so the demo shows true semantic (cosine) ranking, not the
	// lexical fallback that "partial":true returns while vectors are still building.
	Ctx.Insert("TEmbed", Ms());
	SelfTestCtx = Ctx;
	SelfTestWaitTicks = 0;
	AttachIdleHandler("SelfTest_SearchTick", 5, True);

EndProcedure

&AtClient
Procedure SelfTest_SearchTick() Export

	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_StatsEnd", ThisObject, SelfTestCtx), "stats", "{}");

EndProcedure

&AtClient
Procedure SelfTest_StatsEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestWaitTicks = SelfTestWaitTicks + 1;
	SelfTestAppend(Ctx, "stats[" + String(SelfTestWaitTicks) + "] -> " + Left(ResultJson, 300));

	// Keep waiting while vectors are still building (cap the wait so a stuck
	// embedder can't hang the run; the launcher's deadline is the backstop).
	Ready = (StrFind(ResultJson, """vector_status"":""ready""") > 0);
	If Not Ready And SelfTestWaitTicks < 18 Then
		AttachIdleHandler("SelfTest_SearchTick", 5, True);
		Return;
	EndIf;

	SelfTestAppend(Ctx, "steps embed (300 segments) took " + String(Ms() - Ctx.TEmbed)
		+ " ms; vectors " + ?(Ready, "ready", "wait-timeout") + " after "
		+ String(SelfTestWaitTicks) + " ticks; searching");
	Sp = New Structure;
	Sp.Insert("query", "удаление пользователя");
	Sp.Insert("collection", "steps");
	Sp.Insert("mode", "hybrid");
	Sp.Insert("k", 5);
	Sp.Insert("include_text", True);
	Ctx.Insert("TSearch", Ms());
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_SearchEnd", ThisObject, Ctx),
		"search", SerializeToJson(Sp));

EndProcedure

&AtClient
Procedure SelfTest_SearchEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "steps search latency = " + String(Ms() - Ctx.TSearch) + " ms");
	SelfTestAppend(Ctx, "steps search -> " + ResultJson);

	// Phase 2: index the real IRP Gherkin features (scenario titles) as a second
	// collection and search them too — proves the chain on a second real corpus.
	FeatJson = BuildFeaturesSegmentsJSON("features", 400);
	SelfTestAppend(Ctx, "indexing feature segments (" + String(StrLen(FeatJson)) + " bytes)");
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_FeatIndexEnd", ThisObject, Ctx),
		"index_segments", FeatJson);

EndProcedure

&AtClient
Procedure SelfTest_FeatIndexEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "features index_segments -> " + Left(ResultJson, 200));
	Ctx.Insert("TEmbedF", Ms());
	SelfTestCtx = Ctx;
	SelfTestWaitTicks = 0;
	AttachIdleHandler("SelfTest_FeatTick", 5, True);

EndProcedure

&AtClient
Procedure SelfTest_FeatTick() Export

	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_FeatStatsEnd", ThisObject, SelfTestCtx), "stats", "{}");

EndProcedure

&AtClient
Procedure SelfTest_FeatStatsEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestWaitTicks = SelfTestWaitTicks + 1;
	SelfTestAppend(Ctx, "features stats[" + String(SelfTestWaitTicks) + "] -> " + Left(ResultJson, 320));

	// Wait until no collection is still "building" (steps is already ready).
	Building = (StrFind(ResultJson, """vector_status"":""building""") > 0);
	If Building And SelfTestWaitTicks < 18 Then
		AttachIdleHandler("SelfTest_FeatTick", 5, True);
		Return;
	EndIf;

	SelfTestAppend(Ctx, "features embed (400 titles) took " + String(Ms() - Ctx.TEmbedF)
		+ " ms; vectors " + ?(Building, "wait-timeout", "ready")
		+ " after " + String(SelfTestWaitTicks) + " ticks; searching");
	Sp = New Structure;
	Sp.Insert("query", "filter documents by company");
	Sp.Insert("collection", "features");
	Sp.Insert("mode", "hybrid");
	Sp.Insert("k", 5);
	Sp.Insert("include_text", True);
	Ctx.Insert("TSearchF", Ms());
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_FeatSearchEnd", ThisObject, Ctx),
		"search", SerializeToJson(Sp));

EndProcedure

&AtClient
Procedure SelfTest_FeatSearchEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "features (titles) search latency = " + String(Ms() - Ctx.TSearchF) + " ms");
	SelfTestAppend(Ctx, "features search -> " + ResultJson);

	// Phase 3: segment the WHOLE features folder BY SCENARIO (one scenario = one
	// segment, verbatim text + meta) and semantic-search it. Sent as a SINGLE
	// index_segments batch so fastembed embeds it with internal rayon + ONNX
	// parallelism — far faster than 340 tiny per-file index_raw jobs.
	SelfTest_ScenStart(Ctx);

EndProcedure

&AtClient
Function ScenBatchJson(Collection, DocId, Slice)
	Payload = New Structure;
	Payload.Insert("collection", Collection);
	Payload.Insert("doc_id", DocId);
	Payload.Insert("name", "IRP scenarios");
	Payload.Insert("segments", Slice);
	W = New JSONWriter;
	W.SetString();
	WriteJSON(W, Payload);
	Return W.Close();
EndFunction

&AtClient
Procedure SelfTest_ScenStart(Ctx)

	// Whole folder, NO cap. Submit in batches of 500 so (a) the worker reports
	// incremental progress (embedded climbs per batch -> percent), and (b) peak
	// memory stays bounded (one giant batch padded ONNX tensors to ~10 GB).
	ScenAll = BuildAllScenarioSegments(100);
	ScenSkip = 0;
	ScenBatchNo = 0;
	Ctx.Insert("ScenTotal", ScenAll.Count());
	Ctx.Insert("TScenIndex", Ms());
	SelfTestAppend(Ctx, "PHASE 3: " + String(ScenAll.Count())
		+ " scenarios (test subfolder, cap 100), batches of 500");
	SelfTest_ScenSubmitNext(Ctx);

EndProcedure

&AtClient
Procedure SelfTest_ScenSubmitNext(Ctx)

	Total = ScenAll.Count();
	If ScenSkip >= Total Then
		SelfTestAppend(Ctx, "all " + String(Total) + " scenarios submitted in "
			+ String(Ms() - Ctx.TScenIndex) + " ms (accept); embedding...");
		Ctx.Insert("TScenEmbed", Ms());
		SelfTestCtx = Ctx;
		SelfTestWaitTicks = 0;
		AttachIdleHandler("SelfTest_ScenTick", 5, True);
		Return;
	EndIf;

	BatchSize = 500;
	Upper = ScenSkip + BatchSize;
	If Upper > Total Then
		Upper = Total;
	EndIf;
	Slice = New Array;
	For Idx = ScenSkip To Upper - 1 Do
		Slice.Add(ScenAll[Idx]);
	EndDo;
	ScenBatchNo = ScenBatchNo + 1;
	ScenSkip = Upper;
	Json = ScenBatchJson("features_scenarios", "irp-scen-" + Format(ScenBatchNo, "NG=0"), Slice);
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_ScenSubmitEnd", ThisObject, Ctx),
		"index_segments", Json);

EndProcedure

&AtClient
Procedure SelfTest_ScenSubmitEnd(ResultJson, ParametersCall, Ctx) Export

	If ScenBatchNo % 4 = 0 Then
		SelfTestAppend(Ctx, "  submitted " + String(ScenSkip) + "/" + String(Ctx.ScenTotal) + " scenarios");
	EndIf;
	SelfTest_ScenSubmitNext(Ctx);

EndProcedure

&AtClient
Procedure SelfTest_ScenTick() Export

	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_ScenStatsEnd", ThisObject, SelfTestCtx), "stats", "{}");

EndProcedure

&AtClient
Procedure SelfTest_ScenStatsEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestWaitTicks = SelfTestWaitTicks + 1;

	Emb = 0;
	Total = 0;
	VecStatus = "";
	Try
		R = New JSONReader;
		R.SetString(ResultJson);
		Obj = ReadJSON(R, True);
		Coll = Obj["result"]["collections"]["features_scenarios"];
		If Coll <> Undefined Then
			Emb = Coll["embedded"];
			Total = Coll["n_segments"];
			VecStatus = Coll["vector_status"];
		EndIf;
	Except
	EndTry;

	Pct = ?(Total > 0, Int(Emb * 100 / Total), 0);
	Elapsed = Ms() - Ctx.TScenEmbed;
	Rate = ?(Elapsed > 0, Int(Emb * 1000 / Elapsed), 0);
	SelfTestAppend(Ctx, "scenarios embedding " + String(Emb) + "/" + String(Total)
		+ " (" + String(Pct) + "%) ~" + String(Rate) + " seg/s, "
		+ String(Int(Elapsed / 1000)) + "s");
	ShowEmbedProgress("Эмбеддинг сценариев", Emb, Total);

	If VecStatus <> "ready" And SelfTestWaitTicks < 170 Then
		AttachIdleHandler("SelfTest_ScenTick", 5, True);
		Return;
	EndIf;

	SelfTestAppend(Ctx, "scenarios embed DONE: " + String(Emb) + " segments in "
		+ String(Elapsed) + " ms (~" + String(Rate) + " seg/s); searching");
	Sp = New Structure;
	Sp.Insert("query", "filter payment documents by company");
	Sp.Insert("collection", "features_scenarios");
	Sp.Insert("mode", "hybrid");
	Sp.Insert("k", 5);
	Sp.Insert("include_text", True);
	Ctx.Insert("TScenSearch", Ms());
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_ScenSearchEnd", ThisObject, Ctx),
		"search", SerializeToJson(Sp));

EndProcedure

&AtClient
Procedure SelfTest_ScenSearchEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "scenarios search latency = " + String(Ms() - Ctx.TScenSearch) + " ms");
	SelfTestAppend(Ctx, "scenarios search -> " + ResultJson);
	SelfTestAppend(Ctx, "DONE");

EndProcedure

&AtClient
Procedure SelfTestAppend(Ctx, Line)

	Ctx.Log.Add(Line);
	WriteSelfTestResult(Ctx.Log);

EndProcedure

&AtServer
Function BuildStepsSegmentsJSON(Collection, MaxSegments)

	Path = "D:\GitHub\MCP-DB-Client\test\steps.json";
	Segments = New Array;
	Try
		Reader = New TextReader(Path, TextEncoding.UTF8);
		Content = Reader.Read();
		Reader.Close();
		JsonReader = New JSONReader;
		JsonReader.SetString(Content);
		Steps = ReadJSON(JsonReader, True);
		For Each StepItem In Steps Do
			If Segments.Count() >= MaxSegments Then
				Break;
			EndIf;
			Name = "";
			Descr = "";
			If TypeOf(StepItem) = Type("Map") Then
				Name = StepItem["ИмяШага"];
				Descr = StepItem["ОписаниеШага"];
			EndIf;
			SegText = String(Name);
			If ValueIsFilled(Descr) Then
				SegText = SegText + " — " + String(Descr);
			EndIf;
			If ValueIsFilled(SegText) Then
				Segments.Add(New Structure("text", SegText));
			EndIf;
		EndDo;
	Except
		// best-effort: index whatever parsed
	EndTry;

	Payload = New Structure;
	Payload.Insert("collection", Collection);
	Payload.Insert("doc_id", "vanessa-steps");
	Payload.Insert("name", "Vanessa steps");
	Payload.Insert("segments", Segments);

	JsonWriter = New JSONWriter;
	JsonWriter.SetString();
	WriteJSON(JsonWriter, Payload);
	Return JsonWriter.Close();

EndFunction

&AtServer
Function BuildFeaturesSegmentsJSON(Collection, MaxSegments)

	Dir = "C:\Users\DitriX\Downloads\IRP-develop\features";
	Segments = New Array;
	Try
		Files = FindFiles(Dir, "*.feature", True);
		For Each File In Files Do
			If Segments.Count() >= MaxSegments Then
				Break;
			EndIf;
			If Not File.IsFile() Then
				Continue;
			EndIf;
			Reader = New TextReader(File.FullName, TextEncoding.UTF8);
			Line = Reader.ReadLine();
			While Line <> Undefined Do
				If Segments.Count() >= MaxSegments Then
					Break;
				EndIf;
				Trimmed = TrimAll(Line);
				Title = "";
				If StrStartsWith(Trimmed, "Scenario Outline:") Then
					Title = TrimAll(Mid(Trimmed, StrLen("Scenario Outline:") + 1));
				ElsIf StrStartsWith(Trimmed, "Scenario:") Then
					Title = TrimAll(Mid(Trimmed, StrLen("Scenario:") + 1));
				ElsIf StrStartsWith(Trimmed, "Структура сценария:") Then
					Title = TrimAll(Mid(Trimmed, StrLen("Структура сценария:") + 1));
				ElsIf StrStartsWith(Trimmed, "Сценарий:") Then
					Title = TrimAll(Mid(Trimmed, StrLen("Сценарий:") + 1));
				EndIf;
				If ValueIsFilled(Title) Then
					Segments.Add(New Structure("text", Title));
				EndIf;
				Line = Reader.ReadLine();
			EndDo;
			Reader.Close();
		EndDo;
	Except
		// best-effort: index whatever parsed
	EndTry;

	Payload = New Structure;
	Payload.Insert("collection", Collection);
	Payload.Insert("doc_id", "irp-features");
	Payload.Insert("name", "IRP features");
	Payload.Insert("segments", Segments);

	JsonWriter = New JSONWriter;
	JsonWriter.SetString();
	WriteJSON(JsonWriter, Payload);
	Return JsonWriter.Close();

EndFunction

&AtServer
Function IsScenarioLine(Trimmed)
	Return StrStartsWith(Trimmed, "Scenario:") Or StrStartsWith(Trimmed, "Scenario Outline:")
		Or StrStartsWith(Trimmed, "Сценарий:") Or StrStartsWith(Trimmed, "Структура сценария:");
EndFunction

&AtServer
Function IsFeatureLine(Trimmed)
	Return StrStartsWith(Trimmed, "Feature:") Or StrStartsWith(Trimmed, "Функционал:")
		Or StrStartsWith(Trimmed, "Функциональность:");
EndFunction

&AtServer
Function HeaderTitle(Trimmed)
	Pos = StrFind(Trimmed, ":");
	If Pos > 0 Then
		Return TrimAll(Mid(Trimmed, Pos + 1));
	EndIf;
	Return Trimmed;
EndFunction

&AtServer
Procedure FlushScenario(Segments, BlockLines, FeatureName, RelPath, MaxSegments)
	If Segments.Count() >= MaxSegments Then
		Return;
	EndIf;
	Text = TrimAll(StrConcat(BlockLines, Chars.LF));
	If Not ValueIsFilled(Text) Then
		Return;
	EndIf;
	If StrLen(Text) > 2000 Then
		Text = Left(Text, 2000);
	EndIf;
	Meta = New Structure;
	Meta.Insert("type", "scenario");
	Meta.Insert("feature", FeatureName);
	Meta.Insert("file", RelPath);
	Seg = New Structure;
	Seg.Insert("text", Text);
	Seg.Insert("meta", Meta);
	Segments.Add(Seg);
EndProcedure

&AtServer
Function BuildAllScenarioSegments(MaxSegments)

	// A small, coherent test subfolder (filter scenarios) for fast iteration.
	Dir = "C:\Users\DitriX\Downloads\IRP-develop\features\Internal\_0915 Filters";
	Root = Dir + "\";
	Segments = New Array;
	Try
		Files = FindFiles(Dir, "*.feature", True);
		For Each F In Files Do
			If Segments.Count() >= MaxSegments Then
				Break;
			EndIf;
			If Not F.IsFile() Then
				Continue;
			EndIf;
			RelPath = F.FullName;
			If StrStartsWith(RelPath, Root) Then
				RelPath = Mid(RelPath, StrLen(Root) + 1);
			EndIf;
			FeatureName = RelPath;
			BlockLines = Undefined;
			Reader = New TextReader(F.FullName, TextEncoding.UTF8);
			Line = Reader.ReadLine();
			While Line <> Undefined Do
				Trimmed = TrimAll(Line);
				If IsFeatureLine(Trimmed) Then
					FeatureName = HeaderTitle(Trimmed);
				ElsIf IsScenarioLine(Trimmed) Then
					If BlockLines <> Undefined Then
						FlushScenario(Segments, BlockLines, FeatureName, RelPath, MaxSegments);
					EndIf;
					BlockLines = New Array;
					BlockLines.Add(Line);
				Else
					If BlockLines <> Undefined Then
						BlockLines.Add(Line);
					EndIf;
				EndIf;
				If Segments.Count() >= MaxSegments Then
					Break;
				EndIf;
				Line = Reader.ReadLine();
			EndDo;
			Reader.Close();
			If BlockLines <> Undefined Then
				FlushScenario(Segments, BlockLines, FeatureName, RelPath, MaxSegments);
			EndIf;
		EndDo;
	Except
		// best-effort: index whatever parsed
	EndTry;

	Return Segments;

EndFunction

&AtClient
Procedure WriteSelfTestResult(Out)

	Text = "";
	For Each Line In Out Do
		Text = Text + Line + Chars.LF;
	EndDo;
	Try
		Writer = New TextWriter("D:\GitHub\MCP-DB-Client\rust-core\target\ragselftest-result.txt", TextEncoding.UTF8);
		Writer.Write(Text);
		Writer.Close();
	Except
		// best-effort; nothing to do if the file can't be written
	EndTry;

EndProcedure

#EndRegion


#Region Utilities

&AtClient
Procedure EmptyCallbackHandler(ResultCall, ParametersCall, AdditionalParameters) Export
	// Fire-and-forget callback for BeginCalling* component method calls.
EndProcedure

&AtClient
Function GetArg(Arguments, ParamName)
	
	If TypeOf(Arguments) = Type("Map") Then
		Try
			Return Arguments[ParamName];
		Except
			Return Undefined;
		EndTry;
	ElsIf TypeOf(Arguments) = Type("Structure") Then
		Value = Undefined;
		Arguments.Property(ParamName, Value);
		Return Value;
	Else
		Return Undefined;
	EndIf;
	
	Return Undefined;
	
EndFunction

&AtClient
Function NumberOrDefault(Value, DefaultValue)
	
	If Value = Undefined Then
		Return DefaultValue;
	EndIf;
	
	Return Value;
	
EndFunction

&AtClient
Function ParseJsonArgument(JsonText, DefaultValue)
	
	If Not ValueIsFilled(JsonText) Then
		Return DefaultValue;
	EndIf;
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(JsonText);
		Return ReadJSON(JSONReader, True);
	Except
		Return DefaultValue;
	EndTry;
	
EndFunction

&AtClient
Function SerializeToJson(Value)
	
	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, Value);
	Return JSONWriter.Close();
	
EndFunction

#EndRegion
